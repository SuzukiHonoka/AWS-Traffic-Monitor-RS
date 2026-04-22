use std::process::{Command, Stdio};
use std::time;

use aws_sdk_lightsail::Client;
use aws_sdk_lightsail::types::{InstanceMetricName, MetricStatistic, MetricUnit, OperationStatus};
use aws_smithy_types::DateTime;
use byte_unit::{Byte, UnitType};
use chrono::Local;
use log::{error, info};

use crate::config::{Config, InstanceConfig};
use crate::time_utils::first_and_last_day_of_month;

pub async fn run_monitor(client: &Client, config: &Config, loop_interval: Option<time::Duration>) {
    let metric_names = [InstanceMetricName::NetworkIn, InstanceMetricName::NetworkOut];

    loop {
        let today = Local::now().date_naive();

        let (fdt, ldt, period) = match first_and_last_day_of_month(today) {
            Some(v) => v,
            None => {
                error!("Failed to compute month date range for {today}");
                sleep_or_break!(loop_interval);
            }
        };

        let period_i32 = match i32::try_from(period) {
            Ok(v) => v,
            Err(e) => {
                error!("Period value {period} overflows i32: {e}");
                sleep_or_break!(loop_interval);
            }
        };

        info!("----------");
        for instance in &config.instance_list {
            check_instance(client, instance, &metric_names, fdt, ldt, period_i32).await;
        }
        info!("----------");

        match loop_interval {
            Some(interval) => {
                info!("Looping..");
                tokio::time::sleep(interval).await;
            }
            None => break,
        }
    }
}

async fn check_instance(
    client: &Client,
    instance: &InstanceConfig,
    metric_names: &[InstanceMetricName],
    fdt: DateTime,
    ldt: DateTime,
    period: i32,
) {
    info!("Instance: {}", instance.name);

    let metric_sum = match fetch_metric_sum(client, instance, metric_names, fdt, ldt, period).await {
        Some(s) => s,
        None => return,
    };

    let byte = match Byte::from_f64(metric_sum) {
        Some(b) => b,
        None => {
            error!("Total metric sum {metric_sum} is not a valid byte count (instance '{}')", instance.name);
            return;
        }
    };
    info!("Total Used: {:.2}", byte.get_appropriate_unit(UnitType::Decimal));

    if instance.limit.is_empty() {
        return;
    }

    let limit_byte = match Byte::parse_str(&instance.limit, false) {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to parse limit '{}' for instance '{}': {e}", instance.limit, instance.name);
            return;
        }
    };

    if byte >= limit_byte {
        info!("Limit: {:.2} reached, current usage: {byte}", limit_byte.get_appropriate_unit(UnitType::Decimal));
        execute_commands(&instance.command_list);
        stop_instance(client, &instance.name).await;
    } else {
        let limit_u64 = limit_byte.as_u64();
        let remaining = Byte::from_u64(limit_u64.saturating_sub(byte.as_u64()));
        let percentage = (remaining.as_u64() as f64 / limit_u64 as f64) * 100.0;
        info!(
            "Traffic Left: {:.2} ({:.2}%)",
            remaining.get_appropriate_unit(UnitType::Decimal),
            percentage
        );
    }
}

/// Fetches NetworkIn + NetworkOut for the instance and returns their sum, or `None` on any error.
async fn fetch_metric_sum(
    client: &Client,
    instance: &InstanceConfig,
    metric_names: &[InstanceMetricName],
    fdt: DateTime,
    ldt: DateTime,
    period: i32,
) -> Option<f64> {
    let mut total: f64 = 0.0;

    for metric_name in metric_names {
        let output = match client
            .get_instance_metric_data()
            .instance_name(instance.name.as_str())
            .metric_name(metric_name.clone())
            .start_time(fdt)
            .end_time(ldt)
            .unit(MetricUnit::Bytes)
            .statistics(MetricStatistic::Sum)
            .period(period)
            .send()
            .await
        {
            Ok(o) => o,
            Err(e) => {
                error!("Failed to get metric data for instance '{}': {e}", instance.name);
                return None;
            }
        };

        let sum = match output
            .metric_data
            .and_then(|m| m.into_iter().next())
            .and_then(|m| m.sum)
        {
            Some(s) => s,
            None => {
                error!("No sum data returned for instance '{}'", instance.name);
                return None;
            }
        };

        let adjusted = match Byte::from_f64(sum) {
            Some(b) => b.get_appropriate_unit(UnitType::Decimal),
            None => {
                error!("Metric value {sum} is not a valid byte count (instance '{}')", instance.name);
                return None;
            }
        };

        info!("Metric: {} Used: {:.2}", metric_name, adjusted);
        total += sum;
    }

    Some(total)
}

fn execute_commands(command_list: &[String]) {
    for command in command_list {
        if command.is_empty() {
            continue;
        }
        info!("Executing command: {command}");

        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match Command::new(parts[0])
            .args(&parts[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(_) => {}
            Err(e) => error!("Failed to execute '{command}': {e}"),
        }
    }
}

async fn stop_instance(client: &Client, instance_name: &str) {
    let stop_output = match client
        .stop_instance()
        .instance_name(instance_name)
        .force(true)
        .send()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            error!("Failed to stop instance '{instance_name}': {e}");
            return;
        }
    };

    match stop_output
        .operations
        .as_ref()
        .and_then(|ops| ops.first())
        .and_then(|op| op.status.as_ref())
    {
        None => error!("No operations returned after stopping instance '{instance_name}'"),
        Some(status) if *status == OperationStatus::Failed => {
            error!("Stop operation failed for instance '{instance_name}'");
        }
        Some(_) => {}
    }
}

/// Sleeps for the loop interval and continues, or breaks if there is no interval.
/// Must be used inside the `loop {}` in `run_monitor`.
macro_rules! sleep_or_break {
    ($interval:expr) => {
        match $interval {
            Some(d) => {
                tokio::time::sleep(d).await;
                continue;
            }
            None => break,
        }
    };
}
use sleep_or_break;
