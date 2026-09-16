//! Optional standalone entry point; ai-monitor embeds the server directly.
use herdr_usage::{db_path, plans::PlanOptions, web::server::Server};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut port = 19999u16;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => port = args.next().ok_or("--port requires a value")?.parse()?,
            "--help" | "-h" => {
                println!(
                    "usage-web [--port 19999]\n本地 token 统计网页；数据库由 HERDR_USAGE_DB 指定。"
                );
                return Ok(());
            }
            _ => return Err(format!("未知参数：{arg}").into()),
        }
    }
    let database = db_path();
    let server = Server::start(database.clone(), port, PlanOptions::default())?;
    println!(
        "Token 统计网页：http://{}\n数据库：{}",
        server.address(),
        database.display()
    );
    loop {
        std::thread::park();
    }
}
