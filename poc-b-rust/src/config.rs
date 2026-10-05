use clap::Parser;

#[derive(Debug, Parser)]
#[command(author, version, about = "MeshSExcel PoC B")]
pub struct Config {
    #[arg(long)]
    pub node_id: String,

    #[arg(long)]
    pub db_dir: String,

    #[arg(long, default_value_t = 20001)]
    pub port: u16,
}

impl Config {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
