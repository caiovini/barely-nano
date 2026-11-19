mod generator;
mod peer_session;
mod rpc_monitor;

use std::{net::SocketAddr, str::FromStr, time::Instant};

use anyhow::{bail, Context, Result};
use generator::{BlockGenerator, Strategy};
use peer_session::PeerSession;
use rpc_monitor::RpcMonitor;
use rsnano_core::{Amount, BlockHash, Networks, DEV_GENESIS_KEY};
use rsnano_ledger::DEV_GENESIS_BLOCK;
use rsnano_work::WorkThresholds;

#[derive(Debug)]
struct Args {
    target: String,
    rpc: String,
    network: String,
    blocks: usize,
    duration_secs: Option<u64>,
    rate: usize,
    accounts: usize,
    batch_size: usize,
    strategy: Strategy,
    zero_pow: bool,
    dry_run: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            target: "192.168.1.200:7075".to_string(),
            rpc: "http://192.168.1.200:7076".to_string(),
            network: "dev".to_string(),
            blocks: 1000,
            duration_secs: None,
            rate: 500,
            accounts: 1,
            batch_size: 200,
            strategy: Strategy::Chained,
            zero_pow: false,
            dry_run: false,
        }
    }
}

impl Args {
    fn parse() -> Result<Self> {
        let mut args = Self::default();
        let mut iter = std::env::args().skip(1);

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "-t" | "--target" => {
                    args.target = iter.next().context("Missing value for --target")?;
                }
                "--rpc" => {
                    args.rpc = iter.next().context("Missing value for --rpc")?;
                }
                "-n" | "--network" => {
                    args.network = iter.next().context("Missing value for --network")?;
                }
                "-b" | "--blocks" => {
                    args.blocks = iter
                        .next()
                        .context("Missing value for --blocks")?
                        .parse()
                        .context("Invalid number for --blocks")?;
                }
                "-d" | "--duration" => {
                    args.duration_secs = Some(
                        iter.next()
                            .context("Missing value for --duration")?
                            .parse()
                            .context("Invalid number for --duration")?,
                    );
                }
                "-r" | "--rate" => {
                    args.rate = iter
                        .next()
                        .context("Missing value for --rate")?
                        .parse()
                        .context("Invalid number for --rate")?;
                }
                "-a" | "--accounts" => {
                    args.accounts = iter
                        .next()
                        .context("Missing value for --accounts")?
                        .parse()
                        .context("Invalid number for --accounts")?;
                }
                "--batch-size" => {
                    args.batch_size = iter
                        .next()
                        .context("Missing value for --batch-size")?
                        .parse()
                        .context("Invalid number for --batch-size")?;
                }
                "--strategy" => {
                    let s = iter.next().context("Missing value for --strategy")?;
                    args.strategy = Strategy::from_str(&s).map_err(|e| anyhow::anyhow!(e))?;
                }
                "--zero-pow" => {
                    args.zero_pow = true;
                }
                "--dry-run" => {
                    args.dry_run = true;
                }
                "-h" | "--help" => {
                    println!("Nano High-Throughput Block Stress Test Tool\n");
                    println!("Usage: nano-stress [OPTIONS]\n");
                    println!("Options:");
                    println!("  -t, --target <ADDR>       Peering target host:port [default: 192.168.1.200:7075]");
                    println!("      --rpc <URL>           RPC endpoint of target node [default: http://192.168.1.200:7076]");
                    println!("  -n, --network <NET>       Network variant (dev, live, beta, test) [default: dev]");
                    println!("  -b, --blocks <COUNT>      Total number of blocks to generate and send [default: 1000]");
                    println!("  -r, --rate <BPS>          Target block rate in blocks/sec (0 for unlimited) [default: 500]");
                    println!("  -a, --accounts <COUNT>    Number of independent accounts [default: 1]");
                    println!("      --batch-size <SIZE>   Number of blocks per generated batch [default: 200]");
                    println!("      --strategy <STRAT>    Block strategy (chained, change) [default: chained]");
                    println!("      --zero-pow            Set PoW difficulty to 0");
                    println!("      --dry-run             Benchmark block generation locally without network transmission");
                    println!("  -h, --help                Print help");
                    std::process::exit(0);
                }
                other => bail!("Unknown argument: {}", other),
            }
        }

        Ok(args)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse()?;
    let network = Networks::from_str(&args.network).map_err(|e| anyhow::anyhow!(e))?;

    println!("============================================================");
    println!("  NANO NETWORK STRESS TESTING TOOL                          ");
    println!("============================================================");
    println!(" Target Peering Port: {}", args.target);
    println!(" RPC Endpoint:        {}", args.rpc);
    println!(" Network:             {:?}", network);
    println!(" Target Block Count:  {}", args.blocks);
    println!(" Active Accounts:     {}", args.accounts);
    println!(
        " Target Send Rate:    {}",
        if args.rate == 0 {
            "UNLIMITED (MAX BURST)".to_string()
        } else {
            format!("{} BPS", args.rate)
        }
    );
    println!(" Block Strategy:      {:?}", args.strategy);
    println!(
        " PoW Difficulty:      {}",
        if args.zero_pow {
            "0 (Zero PoW)".to_string()
        } else {
            format!(
                "0x{:016x} (Dev Threshold)",
                WorkThresholds::publish_dev().threshold_base()
            )
        }
    );
    println!("============================================================\n");

    let rpc = RpcMonitor::new(&args.rpc);

    println!("[1/4] Checking target node via RPC ({})...", args.rpc);
    match rpc.get_version().await {
        Ok(v) => {
            let node_net = v.network.unwrap_or_else(|| "unknown".into());
            let vendor = v.node_vendor.unwrap_or_else(|| "unknown".into());
            println!(
                "  -> Node online! Vendor: {}, Current Network: {}",
                vendor, node_net
            );

            if node_net != args.network {
                println!(
                    "  [!] WARNING: Node is running in '{}' network, but stress tool is configured for '{}'!",
                    node_net, args.network
                );
            }
        }
        Err(e) => {
            println!("  -> Could not reach RPC at {}: {}", args.rpc, e);
            println!("  [!] Continuing anyway, blocks will be sent directly to port 7075.");
        }
    }

    let mut initial_count = rpc.get_block_count().await.ok();
    if initial_count.is_none() {
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        initial_count = rpc.get_block_count().await.ok();
    }
    if let Some(ref c) = initial_count {
        println!(
            "  -> Initial Block Count: {} (Cemented: {}, Unchecked: {})",
            c.count, c.cemented, c.unchecked
        );
    }

    if args.dry_run {
        println!(
            "\n[DRY RUN] Generating {} blocks locally across {} account(s)...",
            args.blocks, args.accounts
        );
        let mut blk_gen = if args.zero_pow {
            BlockGenerator::with_zero_pow()
        } else {
            BlockGenerator::default()
        };
        blk_gen.init_accounts(args.accounts);

        let start = Instant::now();
        let batch = blk_gen.generate_batch(args.blocks, args.strategy);
        let total_time = start.elapsed();
        let bps = (batch.blocks.len() as f64) / total_time.as_secs_f64();

        println!(
            "  -> Successfully generated {} blocks in {:.2?}",
            batch.blocks.len(),
            total_time
        );
        println!("  -> Local Generation Speed: {:.1} blocks/second", bps);
        return Ok(());
    }

    let target_addr: SocketAddr = args
        .target
        .parse()
        .with_context(|| format!("Invalid target socket address '{}'", args.target))?;
    let genesis_hash: BlockHash = DEV_GENESIS_BLOCK.hash();

    let mut generator = if args.zero_pow {
        BlockGenerator::with_zero_pow()
    } else {
        BlockGenerator::default()
    };

    let genesis_acc = DEV_GENESIS_KEY.account().encode_account();
    if let Ok(info) = rpc.get_account_info(&genesis_acc).await {
        if let (Ok(frontier_hash), Ok(balance_u128)) = (
            BlockHash::decode_hex(&info.frontier),
            info.balance.parse::<u128>(),
        ) {
            let balance_raw = Amount::raw(balance_u128);
            println!(
                "  -> Synchronized with ledger frontier: {} (Height: {}, Balance: {:?})",
                frontier_hash, info.block_count, balance_raw
            );
            generator.set_frontier(frontier_hash, balance_raw);
        }
    }
    generator.init_accounts(args.accounts);

    if let Some(duration_secs) = args.duration_secs {
        println!(
            "\n[2/4] Connecting to Nano node peering socket on {}...",
            target_addr
        );
        let mut session = PeerSession::connect(target_addr, network, genesis_hash)
            .await
            .context("P2P Connection or Handshake Failed")?;
        let target_duration = std::time::Duration::from_secs(duration_secs);
        let init_c: u64 = initial_count
            .as_ref()
            .and_then(|c| c.count.parse().ok())
            .unwrap_or(1);
        println!(
            "  -> Continuous duration mode: streaming for {}s (batch_size={}, rate={})",
            duration_secs, args.batch_size, args.rate
        );
        let overall_start = Instant::now();
        let mut last_log = Instant::now();
        let mut last_logged_count = init_c;
        let mut expected_count = init_c;
        let mut total_sent: u64 = 0;

        while overall_start.elapsed() < target_duration {
            let batch = generator.generate_batch(args.batch_size, args.strategy);
            match session.send_blocks(&batch.blocks, args.rate).await {
                Ok((sent, _dur)) => {
                    total_sent += sent as u64;
                    expected_count += sent as u64;
                }
                Err(e) => {
                    println!("  [!] Send error ({}), reconnecting P2P session...", e);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    if let Ok(new_sess) = PeerSession::connect(target_addr, network, genesis_hash).await {
                        session = new_sess;
                    }
                    if let Ok(info) = rpc.get_account_info(&genesis_acc).await {
                        if let (Ok(fh), Ok(bal)) = (
                            BlockHash::decode_hex(&info.frontier),
                            info.balance.parse::<u128>(),
                        ) {
                            generator.set_frontier(fh, Amount::raw(bal));
                            if let Ok(bc) = info.block_count.parse::<u64>() {
                                expected_count = bc;
                            }
                        }
                    }
                    continue;
                }
            }

            // Backpressure check: keep in-flight blocks <= batch_size so queues never overflow
            let wait_bp = Instant::now();
            let mut synced = false;
            while wait_bp.elapsed() < std::time::Duration::from_secs(5) {
                if let Ok(c) = rpc.get_block_count().await {
                    let current_c: u64 = c.count.parse().unwrap_or(0);
                    if current_c + (args.batch_size as u64) >= expected_count {
                        synced = true;
                        if last_log.elapsed() >= std::time::Duration::from_secs(30) {
                            let dt = last_log.elapsed().as_secs_f64();
                            let window_bps = (current_c.saturating_sub(last_logged_count)) as f64 / dt;
                            let avg_bps = (current_c.saturating_sub(init_c)) as f64
                                / overall_start.elapsed().as_secs_f64().max(0.001);
                            println!(
                                "  [t={:4}s / {}s] sent={} | ledger={} (cemented={}, unchecked={}) | window={:.1} bps | avg={:.1} bps",
                                overall_start.elapsed().as_secs(),
                                duration_secs,
                                total_sent,
                                current_c,
                                c.cemented,
                                c.unchecked,
                                window_bps,
                                avg_bps
                            );
                            last_log = Instant::now();
                            last_logged_count = current_c;
                        }
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            if !synced {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                if let Ok(info) = rpc.get_account_info(&genesis_acc).await {
                    if let (Ok(fh), Ok(bal)) = (
                        BlockHash::decode_hex(&info.frontier),
                        info.balance.parse::<u128>(),
                    ) {
                        generator.set_frontier(fh, Amount::raw(bal));
                        if let Ok(bc) = info.block_count.parse::<u64>() {
                            expected_count = bc;
                        }
                    }
                }
            }
        }

        println!(
            "\n[4/4] Duration {}s reached! Waiting for final blocks to commit (total_sent={})...",
            duration_secs, total_sent
        );
        let wait_end = Instant::now();
        while wait_end.elapsed() < std::time::Duration::from_secs(15) {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if let Ok(c) = rpc.get_block_count().await {
                let current_c: u64 = c.count.parse().unwrap_or(0);
                if current_c >= expected_count {
                    break;
                }
            }
        }
        if let Ok(final_count) = rpc.get_block_count().await {
            let final_c: u64 = final_count.count.parse().unwrap_or(0);
            let growth = final_c.saturating_sub(init_c);
            let e2e_dur = overall_start.elapsed();
            let e2e_bps = (growth as f64) / e2e_dur.as_secs_f64().max(0.001);
            println!(
                "  -> Final Block Count: {} (Cemented: {}, Unchecked: {}) | Growth: +{} blocks in {:.1?} ({:.1} bps)",
                final_count.count, final_count.cemented, final_count.unchecked, growth, e2e_dur, e2e_bps
            );
        }
        return Ok(());
    }

    let gen_start = Instant::now();
    let mut batches = Vec::new();
    let mut rem_gen = args.blocks;
    while rem_gen > 0 {
        let chunk = rem_gen.min(args.batch_size);
        batches.push(generator.generate_batch(chunk, args.strategy));
        rem_gen -= chunk;
    }
    println!(
        "  -> Pre-generated {} blocks in {:.2?} ({} batches)",
        args.blocks,
        gen_start.elapsed(),
        batches.len()
    );

    println!(
        "\n[2/4] Connecting to Nano node peering socket on {}...",
        target_addr
    );
    let mut session = PeerSession::connect(target_addr, network, genesis_hash)
        .await
        .context("P2P Connection or Handshake Failed")?;

    println!(
        "\n[3/4] Streaming {} blocks at {}...",
        args.blocks,
        if args.rate == 0 {
            "unlimited burst".to_string()
        } else {
            format!("{} bps", args.rate)
        }
    );

    let overall_start = Instant::now();
    let mut total_sent = 0;

    for batch in &batches {
        let (sent, dur) = session.send_blocks(&batch.blocks, args.rate).await?;
        total_sent += sent;

        let bps = (sent as f64) / dur.as_secs_f64().max(0.0001);
        let bytes_sent = sent * 224;
        let kbps = (bytes_sent as f64 / 1024.0) / dur.as_secs_f64().max(0.0001);

        println!(
            "  Sent batch: {} blocks in {:.3?} ({:.0} bps, {:.1} KB/s) | Total: {}/{}",
            sent, dur, bps, kbps, total_sent, args.blocks
        );
    }

    let overall_dur = overall_start.elapsed();
    let overall_bps = (total_sent as f64) / overall_dur.as_secs_f64();
    let total_kb = (total_sent * 224) as f64 / 1024.0;

    println!("\n============================================================");
    println!("  STRESS RUN COMPLETED                                      ");
    println!("============================================================");
    println!(" Total Blocks Sent:   {}", total_sent);
    println!(" Total Data Sent:     {:.2} KB", total_kb);
    println!(" TCP Stream Time:     {:.2?}", overall_dur);
    println!(" TCP Send Throughput: {:.1} blocks/second", overall_bps);
    println!("============================================================\n");

    println!("[4/4] Waiting for node pipeline to process and cement blocks...");
    let expected_count = initial_count
        .as_ref()
        .and_then(|c| c.count.parse::<u64>().ok())
        .map(|c| c + total_sent as u64);

    let wait_start = Instant::now();
    let max_wait = std::time::Duration::from_secs(((total_sent / 250) + 15) as u64);
    let mut final_count = None;
    let mut last_c = 0;
    let mut stable_iterations = 0;

    while wait_start.elapsed() < max_wait {
        if let Ok(c) = rpc.get_block_count().await {
            let current_c: u64 = c.count.parse().unwrap_or(0);
            final_count = Some(c);

            if let Some(expected) = expected_count {
                if current_c >= expected {
                    break;
                }
                if current_c == last_c && current_c > 0 {
                    stable_iterations += 1;
                    if stable_iterations >= 600
                        && wait_start.elapsed() > std::time::Duration::from_secs(30)
                    {
                        break;
                    }
                } else {
                    stable_iterations = 0;
                }
            } else if current_c == last_c && current_c > 0 {
                stable_iterations += 1;
                if stable_iterations >= 60 {
                    break;
                }
            } else {
                stable_iterations = 0;
            }
            last_c = current_c;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    let e2e_dur = overall_start.elapsed();

    if let Some(final_count) = final_count {
        println!(
            "  -> Current Block Count: {} (Cemented: {}, Unchecked: {})",
            final_count.count, final_count.cemented, final_count.unchecked
        );
        if let Some(ref init) = initial_count {
            let init_c: u64 = init.count.parse().unwrap_or(0);
            let final_c: u64 = final_count.count.parse().unwrap_or(0);
            let init_u: u64 = init.unchecked.parse().unwrap_or(0);
            let final_u: u64 = final_count.unchecked.parse().unwrap_or(0);
            let growth = final_c.saturating_sub(init_c);
            let e2e_bps = (growth as f64) / e2e_dur.as_secs_f64().max(0.0001);

            println!("  -> Ledger Block Growth: +{} blocks", growth);
            println!(
                "  -> Unchecked Queue:     +{} blocks",
                final_u.saturating_sub(init_u)
            );
            println!("  -> End-to-End Time:     {:.2?}", e2e_dur);
            println!("  -> Node Commit Rate:    {:.1} blocks/second", e2e_bps);
        }
    }

    Ok(())
}
