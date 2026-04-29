use std::env;
use std::fs::File;
use std::io::stdout;

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    let path = env::args()
        .nth(1)
        .context("usage: payments-engine-challenge <input.csv>")?;
    let file = File::open(&path).with_context(|| format!("open {path}"))?;
    payments_engine_challenge::run(file, stdout().lock())
}
