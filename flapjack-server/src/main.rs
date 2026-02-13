#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use clap::Parser;
use flapjack_http::serve;

#[derive(Parser)]
#[command(name = "flapjack")]
struct Args {
    #[arg(long, env = "FLAPJACK_DATA_DIR", default_value = "./data")]
    data_dir: String,
    #[arg(long, env = "FLAPJACK_BIND_ADDR", default_value = "127.0.0.1:7700")]
    bind_addr: String,
    #[arg(long, default_value = "7700")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    std::env::set_var("FLAPJACK_DATA_DIR", &args.data_dir);
    std::env::set_var("FLAPJACK_BIND_ADDR", &args.bind_addr);
    serve().await
}
