#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--check") => {
            let platform = stabilizer::platform::detect();
            println!("{}", serde_json::to_string_pretty(&platform)?);
            anyhow::ensure!(platform.supported, "{}", platform.reason);
            Ok(())
        }
        Some("--once") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&stabilizer::monitor::read_once()?)?
            );
            Ok(())
        }
        Some("--help") => {
            println!(
                "stabilizer-agent [--check | --once]\nБез параметров: фоновый монитор и применение сохранённых правил.\n--once: только чтение, без изменения правил системы."
            );
            Ok(())
        }
        Some(other) => anyhow::bail!("Неизвестный аргумент: {other}"),
        None => stabilizer::agent::run().await,
    }
}
