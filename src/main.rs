use std::io::Write;
use clap::{value_parser, Arg, Command as ClapCommand};
use serde::{Deserialize, Serialize};
use aws_config::{BehaviorVersion, Region};
use aws_sdk_lightsail::config::Credentials;
use aws_sdk_lightsail::types::{InstanceMetricName, MetricStatistic, MetricUnit, OperationStatus};
use aws_sdk_lightsail::Client;
use aws_smithy_types::DateTime;
use aws_smithy_types_convert::date_time::DateTimeExt;
use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
use byte_unit::{Byte, UnitType};
use log::{info, LevelFilter};
use std::error::Error;
use std::fs::File;
use std::process::Stdio;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time;

const DEFAULT_REGION: &str = "ap-northeast-1";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    loop_interval: Option<u32>,
    instance_list: Vec<InstanceConfig>,
    aws_config: AWSConfig,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstanceConfig {
    name: String,
    limit: String,
    command_list: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AWSConfig {
    //credential_path: String,
    region: Option<String>,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::builder()
        .format(|buf, record| {
            writeln!(
                buf,
                "{} {}",
                Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.args()
            )
        })
        .filter_level(LevelFilter::Trace)
        .init();

    // Build cli arguments
    let matches = ClapCommand::new("AWS Traffic Monitor")
        .version("0.1.0")
        .author("starx")
        .about("Monitor the AWS lightsail traffic metric and perform the commands when condition meets")
        .arg(
            Arg::new("config")
                .short('c')
                .value_parser(value_parser!(PathBuf))
                .default_value("config.json")
                .help("Config file")
        )
        .get_matches();
    let config_path = matches.get_one::<PathBuf>("config").unwrap();

    // Read config file
    let config: Config = read_config_from_file(config_path).expect("Unable to read config file");

    let credentials = Credentials::new(
        config.aws_config.access_key_id,
        config.aws_config.secret_access_key,
        Option::from(config.aws_config.session_token),
        None,
        "manual"
    );
    let shared_config = aws_config::defaults(BehaviorVersion::latest())
        .credentials_provider(credentials)
        .region(Region::new(config.aws_config.region.unwrap_or_else(|| DEFAULT_REGION.to_string())))
        .load().await;

    let client = Client::new(&shared_config);

    let today = Local::now().date_naive();
    let (fdt, ldt, period) = first_and_last_day_of_month(today);

    let metric_names = vec![
        InstanceMetricName::NetworkIn,
        InstanceMetricName::NetworkOut
    ];

    let loop_interval: Option<time::Duration> = config.loop_interval
        .map(|secs| time::Duration::from_secs(secs as u64));

    loop {
        info!("----------");
        for instance in &config.instance_list {
            info!("Instance: {}", instance.name);
            let mut metric_sum: f64 = 0.0;
            for metric_name in &metric_names {
                let input = client.get_instance_metric_data()
                    .instance_name(instance.name.as_str())
                    .metric_name(metric_name.clone())
                    .start_time(fdt)
                    .end_time(ldt)
                    .unit(MetricUnit::Bytes)
                    .statistics(MetricStatistic::Sum)
                    .period(i32::try_from(period)?);
                let output = input.send().await?;

                let sum = match output.metric_data
                    .and_then(|metrics| metrics.into_iter().next())
                    .and_then(|metric| metric.sum) {
                    None => panic!("No sum data found for instance {}", instance.name),
                    Some(sum) => sum
                };
                metric_sum += sum;

                let adjusted = Byte::from_f64(sum).unwrap()
                    .get_appropriate_unit(UnitType::Decimal);
                info!("Metric: {} Used: {:.2}", metric_name, adjusted);
            }

            let byte = Byte::from_f64(metric_sum).unwrap();
            let adjusted = byte.get_appropriate_unit(UnitType::Decimal);
            info!("Total Used: {:.2}", adjusted);

            if !instance.limit.is_empty() {
                let limit_byte = Byte::parse_str(&instance.limit, false)?;
                if byte.ge(&limit_byte) {
                    let adjusted_limit_byte = limit_byte.get_appropriate_unit(UnitType::Decimal);
                    info!("Limit: {adjusted_limit_byte:.2} match, current usage: {byte}");

                    for command in &instance.command_list {
                        if command.is_empty() {
                            continue;
                        }
                        info!("Executing command: {command}");

                        let parts: Vec<&str> = command.split_whitespace().collect();
                        if parts.is_empty() {
                            continue;
                        }
                        let program = parts[0];
                        let args = &parts[1..];

                        let _ = Command::new(program)
                            .args(args)
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status()
                            .expect(format!("Failed to execute {command}").as_str());
                    }

                    let output = client.stop_instance()
                        .instance_name(&instance.name)
                        .force(true)
                        .send().await?;

                    match output.operations
                        .as_ref()
                        .and_then(|ops| ops.first())
                        .and_then(|op| op.status.as_ref()) {
                        None => panic!("No operations found for instance {}", instance.name),
                        Some(status) => {
                            if *status == OperationStatus::Failed {
                                panic!("Operation failed for instance {}", instance.name)
                            }
                        }
                    }

                } else {
                    let byte_u64 = byte.as_u64();
                    let limit_byte_u64 = limit_byte.as_u64();
                    let left = Byte::from_u64(limit_byte_u64 - byte_u64)
                        .get_appropriate_unit(UnitType::Decimal);
                    let percentage = ((limit_byte_u64 - byte_u64) as f64 / limit_byte_u64 as f64) * 100.0;
                    info!("Traffic Left: {:.2} ({:.2}%)", left, percentage);
                }
            }
        }
        info!("----------");
        if let Some(interval) = loop_interval {
            info!("Looping..");
            tokio::time::sleep(interval).await;
            continue;
        }
        break
    }
    Ok(())
}

fn first_and_last_day_of_month(date: NaiveDate) -> (DateTime, DateTime, i64) {
    // The first day of the month is always the 1st day.
    let first_day = NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap();
    let first_day_time = first_day.and_hms_opt(0, 0, 0).unwrap();
    let first_day_time_utc = Utc.from_local_datetime(&first_day_time).single().unwrap();
    let fdt = DateTime::from_chrono_utc(first_day_time_utc);

    // The last day of the month is the day before the first day of the *next* month.
    // To calculate the next month's first day reliably, even for December,
    // we can use a helper function or manual calculation.
    let next_month_first_day = if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)
    }.unwrap();

    // Subtract one day from the first day of the next month to get the last day of the current month.
    let last_day = next_month_first_day - Duration::days(1);
    let last_day_time = last_day.and_hms_opt(23, 59, 59).unwrap();
    let last_day_time_utc = Utc.from_local_datetime(&last_day_time).single().unwrap();
    let ldt = DateTime::from_chrono_utc(last_day_time_utc);

    let period = last_day_time - first_day_time;
    let period_seconds = period.num_seconds() + 1;
    (fdt, ldt, period_seconds)
}

fn read_config_from_file<P: AsRef<Path>>(path: P) -> Result<Config, Box<dyn Error>> {
    // Open the file in read-only mode with buffer.
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    // Read the JSON contents of the file as an instance of `Config`.
    let cfg = serde_json::from_reader(reader)?;

    // Return the `Config`.
    Ok(cfg)
}
