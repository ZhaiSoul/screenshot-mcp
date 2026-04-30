mod capture;
mod dxgi;
mod mcp;
mod wgc;
mod window;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware;

#[derive(Parser)]
#[command(name = "screenshot-mcp", about = "Windows Screenshot MCP Server (double-click to start)", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Clone)]
enum Commands {
    /// Start MCP server (stdio mode)
    Mcp {
        /// Transport: stdio or http
        #[arg(long, default_value = "stdio")]
        transport: String,
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
        #[arg(long, default_value_t = 3210)]
        port: u16,
    },
    Screenshot {
        #[arg(long, default_value = "include")]
        mode: String,
        #[arg(long, action = clap::ArgAction::Append)]
        filter_title: Vec<String>,
        #[arg(long, action = clap::ArgAction::Append)]
        filter_title_exact: Vec<String>,
        #[arg(long, action = clap::ArgAction::Append)]
        filter_title_regex: Vec<String>,
        #[arg(long, action = clap::ArgAction::Append)]
        filter_process: Vec<String>,
        #[arg(long, default_value = "0")]
        monitor: usize,
        #[arg(long, short, default_value = "screenshot.png")]
        output: PathBuf,
        #[arg(long, default_value = "png")]
        format: String,
    },
    ListWindows { #[arg(long)] include_minimized: bool },
    ActiveWindow {},
    ListMonitors,
}

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("screenshot_bot=debug".parse().unwrap()),
        )
        .init();

    unsafe { SetProcessDPIAware(); }

    // Detect double-click (no args) → start HTTP MCP server
    let args: Vec<String> = std::env::args().collect();
    if args.len() <= 1 {
        run_mcp_server("http", "0.0.0.0", 3210);
        return;
    }

    // Has args → parse CLI
    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Mcp { transport, host, port }) => {
            run_mcp_server(transport, host, *port);
        }
        Some(Commands::Screenshot { mode, filter_title, filter_title_exact, filter_title_regex, filter_process, monitor, output, format }) => {
            run_screenshot_cli(mode, filter_title, filter_title_exact, filter_title_regex, filter_process, *monitor, output, format);
        }
        Some(Commands::ListWindows { include_minimized }) => {
            run_list_windows_cli(*include_minimized);
        }
        Some(Commands::ActiveWindow {}) => {
            run_active_window_cli();
        }
        Some(Commands::ListMonitors) => {
            run_list_monitors_cli();
        }
        None => {
            Cli::parse_from(["--help"]);
        }
    }
}

// ── Command handlers ──────────────────────────────────────────────────

fn run_mcp_server(transport: &str, host: &str, port: u16) {
    match transport {
        "http" => {
            eprintln!("Starting MCP HTTP server on http://{}:{} ...", host, port);
            eprintln!("Press Ctrl+C to stop.");
            let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
            rt.block_on(async {
                if let Err(e) = mcp::run_http_server(host, port).await {
                    eprintln!("HTTP server error: {}", e);
                    std::process::exit(1);
                }
            });
        }
        _ => {
            if let Err(e) = mcp::run_server() {
                eprintln!("MCP server error: {}", e);
                std::process::exit(1);
            }
        }
    }
}

fn run_screenshot_cli(
    mode: &str, ft: &[String], fte: &[String], ftr: &[String], fp: &[String],
    mon: usize, out: &PathBuf, fmt: &str,
) {
    let mut rules = Vec::new();
    for t in ft { rules.push(window::FilterRule { rule_type: window::FilterType::TitleContains, value: t.clone() }); }
    for t in fte { rules.push(window::FilterRule { rule_type: window::FilterType::TitleExact, value: t.clone() }); }
    for t in ftr { rules.push(window::FilterRule { rule_type: window::FilterType::TitleRegex, value: t.clone() }); }
    for p in fp { rules.push(window::FilterRule { rule_type: window::FilterType::ProcessName, value: p.clone() }); }

    let filters = if rules.is_empty() { None } else {
        Some(window::WindowFilter {
            mode: match mode { "exclude" => window::FilterMode::Exclude, _ => window::FilterMode::Include },
            rules,
        })
    };
    let img_fmt = match fmt { "jpeg" | "jpg" => capture::ImageFormat::Jpeg, _ => capture::ImageFormat::Png };

    match capture::take_screenshot(mon, filters.as_ref(), img_fmt) {
        Ok(data) => {
            if let Err(e) = std::fs::write(out, &data) { eprintln!("Error writing file: {}", e); std::process::exit(1); }
            eprintln!("Screenshot saved to: {} ({} bytes)", out.display(), data.len());
        }
        Err(e) => { eprintln!("Error taking screenshot: {}", e); std::process::exit(1); }
    }
}

fn truncate_str(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}...", s.chars().take(n - 3).collect::<String>()) } else { s.to_string() }
}

fn run_list_windows_cli(include_min: bool) {
    let wins = window::enumerate_windows(include_min);
    if wins.is_empty() { println!("No windows found."); return; }
    println!("Found {} window(s):\n", wins.len());
    println!("{:<4} {:<10} {:<30} {:<35} {:<8} {:<12}", "#", "HWND", "Process", "Title", "PID", "Position");
    println!("{}", "-".repeat(100));
    for (i, w) in wins.iter().enumerate() {
        println!("{:<4} {:#010X} {:<30} {:<35} {:<8} ({},{}) {}x{}",
            i+1, w.hwnd, truncate_str(&w.process_name, 25), truncate_str(&w.title, 30), w.pid, w.rect.left, w.rect.top, w.rect.width(), w.rect.height());
    }
}

fn run_active_window_cli() {
    match window::get_active_window() {
        Some(w) => {
            println!("Active Window:");
            println!("  Title:   {}", w.title);
            println!("  Process: {} (PID={})", w.process_name, w.pid);
            println!("  Pos:     ({}, {}) {}x{}", w.rect.left, w.rect.top, w.rect.width(), w.rect.height());
        }
        None => { println!("No active window found."); }
    }
}

fn run_list_monitors_cli() {
    let monitors = window::enumerate_monitors();
    if monitors.is_empty() { println!("No monitors found."); return; }
    println!("Found {} monitor(s):\n", monitors.len());
    for m in &monitors {
        println!("  [{}] {} ({},{}) {}x{} {}", m.index, m.name, m.rect.left, m.rect.top, m.rect.width(), m.rect.height(),
            if m.is_primary { "(Primary)" } else { "" });
    }
}
