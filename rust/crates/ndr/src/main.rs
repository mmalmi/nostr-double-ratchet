use clap::{Parser, Subcommand};

mod commands;
mod config;
mod output;
mod storage;

use output::Output;

#[derive(Parser)]
#[command(name = "ndr")]
#[command(version)]
#[command(about = "CLI for encrypted Nostr messaging using double ratchet")]
#[command(long_about = "A command-line tool for end-to-end encrypted messaging over Nostr.\n\nDesigned for humans, AI agents, and automation.")]
struct Cli {
    /// Output in JSON format (for agents/scripts)
    #[arg(short, long, global = true)]
    json: bool,

    /// Data directory (default: platform data dir/ndr)
    #[arg(long, global = true, env = "NDR_DATA_DIR")]
    data_dir: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Login with a private key
    Login {
        /// Private key (nsec or hex)
        key: String,
    },

    /// Logout and clear all data
    Logout,

    /// Show current identity
    Whoami,

    /// Invite management
    #[command(subcommand)]
    Invite(InviteCommands),

    /// Chat management
    #[command(subcommand)]
    Chat(ChatCommands),

    /// Send a message
    Send {
        /// Chat ID
        chat_id: String,
        /// Message content
        message: String,
    },

    /// Read messages from a chat
    Read {
        /// Chat ID
        chat_id: String,
        /// Maximum number of messages to show
        #[arg(short, long, default_value = "50")]
        limit: usize,
    },

    /// Listen for new messages
    Listen {
        /// Specific chat ID (optional, listens to all if not specified)
        #[arg(short, long)]
        chat: Option<String>,
    },
}

#[derive(Subcommand)]
enum InviteCommands {
    /// Create a new invite
    Create {
        /// Label for the invite
        #[arg(short, long)]
        label: Option<String>,
    },

    /// List all invites
    List,

    /// Delete an invite
    Delete {
        /// Invite ID
        id: String,
    },

    /// Listen for invite acceptances
    Listen,
}

#[derive(Subcommand)]
enum ChatCommands {
    /// List all chats
    List,

    /// Join a chat via invite URL
    Join {
        /// Invite URL or hash
        url: String,
    },

    /// Show chat details
    Show {
        /// Chat ID
        id: String,
    },

    /// Delete a chat
    Delete {
        /// Chat ID
        id: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let output = Output::new(cli.json);

    let result = run(cli, &output).await;

    if let Err(e) = result {
        output.error(&e.to_string());
        std::process::exit(1);
    }
}

async fn run(cli: Cli, output: &Output) -> anyhow::Result<()> {
    let data_dir = cli.data_dir.unwrap_or_else(|| {
        dirs::data_dir()
            .expect("Could not find data directory")
            .join("ndr")
    });

    // Ensure data directory exists
    std::fs::create_dir_all(&data_dir)?;

    let config = config::Config::load(&data_dir)?;
    let storage = storage::Storage::open(&data_dir)?;

    match cli.command {
        Commands::Login { key } => {
            commands::identity::login(&key, &config, &storage, output).await
        }
        Commands::Logout => {
            commands::identity::logout(&data_dir, output).await
        }
        Commands::Whoami => {
            commands::identity::whoami(&config, output).await
        }
        Commands::Invite(cmd) => match cmd {
            InviteCommands::Create { label } => {
                commands::invite::create(label, &config, &storage, output).await
            }
            InviteCommands::List => {
                commands::invite::list(&storage, output).await
            }
            InviteCommands::Delete { id } => {
                commands::invite::delete(&id, &storage, output).await
            }
            InviteCommands::Listen => {
                commands::invite::listen(&config, &storage, output).await
            }
        },
        Commands::Chat(cmd) => match cmd {
            ChatCommands::List => {
                commands::chat::list(&storage, output).await
            }
            ChatCommands::Join { url } => {
                commands::chat::join(&url, &config, &storage, output).await
            }
            ChatCommands::Show { id } => {
                commands::chat::show(&id, &storage, output).await
            }
            ChatCommands::Delete { id } => {
                commands::chat::delete(&id, &storage, output).await
            }
        },
        Commands::Send { chat_id, message } => {
            commands::message::send(&chat_id, &message, &config, &storage, output).await
        }
        Commands::Read { chat_id, limit } => {
            commands::message::read(&chat_id, limit, &storage, output).await
        }
        Commands::Listen { chat } => {
            commands::message::listen(chat.as_deref(), &config, &storage, output).await
        }
    }
}
