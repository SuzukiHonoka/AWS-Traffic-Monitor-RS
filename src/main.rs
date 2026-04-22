mod config;
mod monitor;
mod time_utils;

use std::io::Write;
use std::path::PathBuf;
use std::time;

use aws_config::{BehaviorVersion, Region};
use aws_sdk_lightsail::config::Credentials;
use aws_sdk_lightsail::Client;
use chrono::Local;
use clap::{value_parser, Arg, Command as ClapCommand};
use log::LevelFilter;

use config::read_config_from_file;

const DEFAULT_REGION: &str = "ap-northeast-1";

#[tokio::main]
async fn main() {
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

    let matches = ClapCommand::new("AWS Traffic Monitor")
        .version("0.1.0")
        .author("starx")
        .about("Monitor the AWS Lightsail traffic metric and execute commands when the limit is reached")
        .arg(
            Arg::new("config")
                .short('c')
                .value_parser(value_parser!(PathBuf))
                .default_value("config.json")
                .help("Path to the JSON config file"),
        )
        .get_matches();

    // config_path always has a default value, so this unwrap is safe.
    let config_path = matches.get_one::<PathBuf>("config").unwrap();

    let config = match read_config_from_file(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: unable to read config file '{}': {e}", config_path.display());
            std::process::exit(1);
        }
    };

    let credentials = Credentials::new(
        config.aws_config.access_key_id.clone(),
        config.aws_config.secret_access_key.clone(),
        config.aws_config.session_token.clone(),
        None,
        "manual",
    );
    let region = config
        .aws_config
        .region
        .clone()
        .unwrap_or_else(|| DEFAULT_REGION.to_string());

    let shared_config = aws_config::defaults(BehaviorVersion::latest())
        .credentials_provider(credentials)
        .region(Region::new(region))
        .load()
        .await;

    let client = Client::new(&shared_config);

    let loop_interval = config
        .loop_interval
        .map(|secs| time::Duration::from_secs(secs as u64));

    monitor::run_monitor(&client, &config, loop_interval).await;
}