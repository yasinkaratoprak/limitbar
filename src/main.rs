mod loopback_tls;
mod model;
mod providers;
mod render;
mod util;

use clap::{Parser, Subcommand};
use model::Report;
use std::io::IsTerminal;
use std::process::ExitCode;

/// AI usage limits for the providers you are signed in to.
#[derive(Parser)]
#[command(name = "limitbar", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    usage: UsageArgs,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Show usage limits (the default command).
    Usage(UsageArgs),
    /// List supported providers and whether you are signed in.
    Providers,
}

#[derive(clap::Args, Clone, Default)]
struct UsageArgs {
    /// Only these providers (repeatable): codex, claude, gemini, antigravity.
    #[arg(short, long = "provider", value_name = "ID")]
    providers: Vec<String>,
}

/// Output flags, accepted before or after any subcommand.
#[derive(clap::Args, Clone, Default)]
struct OutputArgs {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// Disable ANSI colors.
    #[arg(long, global = true)]
    no_color: bool,
    /// Hide account emails.
    #[arg(long, global = true)]
    hide_account: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Providers) => {
            list_providers(&cli.output);
            ExitCode::SUCCESS
        }
        Some(Command::Usage(args)) => usage(args, &cli.output),
        None => usage(cli.usage, &cli.output),
    }
}

fn list_providers(output: &OutputArgs) {
    let rows: Vec<_> = providers::all()
        .iter()
        .map(|p| (p.id(), p.source(), p.signed_in()))
        .collect();
    if output.json {
        let json: Vec<_> = rows
            .iter()
            .map(|(id, source, signed_in)| {
                serde_json::json!({"provider": id, "source": source, "signed_in": signed_in})
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json).expect("serializable list")
        );
        return;
    }
    for (id, source, signed_in) in rows {
        let state = if signed_in {
            "signed in"
        } else {
            "not signed in"
        };
        println!("{id:<12} {source:<12} {state}");
    }
}

fn usage(args: UsageArgs, output: &OutputArgs) -> ExitCode {
    let known = providers::all();
    for id in &args.providers {
        if !known.iter().any(|p| p.id() == id) {
            let ids: Vec<_> = known.iter().map(|p| p.id()).collect();
            eprintln!("Error: unknown provider '{id}' (known: {})", ids.join(", "));
            return ExitCode::from(2);
        }
    }
    // Without --provider, ask every provider whose credentials are present.
    let selected: Vec<_> = known
        .into_iter()
        .filter(|p| {
            if args.providers.is_empty() {
                p.signed_in()
            } else {
                args.providers.iter().any(|id| id == p.id())
            }
        })
        .collect();

    let mut reports: Vec<Report> = std::thread::scope(|scope| {
        let handles: Vec<_> = selected
            .iter()
            .map(|provider| scope.spawn(move || provider.report()))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("provider thread panicked"))
            .collect()
    });

    if output.hide_account {
        for report in &mut reports {
            if let Report::Ok(usage) = report {
                usage.account = None;
            }
        }
    }

    if output.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&reports).expect("serializable report")
        );
    } else {
        let style = render::Style {
            color: !output.no_color
                && std::io::stdout().is_terminal()
                && std::env::var_os("NO_COLOR").is_none(),
            show_account: !output.hide_account,
        };
        print!("{}", render::text(&reports, &style, chrono::Utc::now()));
    }
    let failed = reports.iter().any(|r| matches!(r, Report::Failed { .. }));
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
