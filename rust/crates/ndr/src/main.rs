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
        /// Chat ID, npub, hex pubkey, or contact name
        target: String,
        /// Message content
        message: String,
    },

    /// React to a message
    React {
        /// Chat ID, npub, hex pubkey, or contact name
        target: String,
        /// Message ID to react to
        message_id: String,
        /// Emoji reaction (e.g., 👍, ❤️, +1)
        emoji: String,
    },

    /// Send a typing indicator
    Typing {
        /// Chat ID, npub, hex pubkey, or contact name
        target: String,
    },

    /// Send a delivery/read receipt
    Receipt {
        /// Chat ID, npub, hex pubkey, or contact name
        target: String,
        /// Receipt type: "delivered" or "seen"
        receipt_type: String,
        /// Message IDs to acknowledge
        message_ids: Vec<String>,
    },

    /// Read messages from a chat
    Read {
        /// Chat ID, npub, hex pubkey, or contact name
        target: String,
        /// Maximum number of messages to show
        #[arg(short, long, default_value = "50")]
        limit: usize,
    },

    /// Manage contacts (petnames)
    #[command(subcommand)]
    Contact(ContactCommands),

    /// Listen for new messages
    Listen {
        /// Specific chat ID (optional, listens to all if not specified)
        #[arg(short, long)]
        chat: Option<String>,
    },

    /// Receive and decrypt a nostr event
    Receive {
        /// The nostr event JSON
        event: String,
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

    /// Process an invite acceptance event (creates chat session)
    Accept {
        /// Invite ID
        invite_id: String,
        /// The acceptance event JSON
        event: String,
    },
}

#[derive(Subcommand)]
enum ContactCommands {
    /// Add a contact (petname)
    Add {
        /// npub or hex pubkey
        pubkey: String,
        /// Petname
        name: String,
    },

    /// List all contacts
    List,

    /// Remove a contact
    Remove {
        /// Petname to remove
        name: String,
    },
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

    let mut config = config::Config::load(&data_dir)?;
    let storage = storage::Storage::open(&data_dir)?;

    // Commands that need identity - auto-generate if not logged in
    let needs_identity = matches!(
        &cli.command,
        Commands::Invite(_)
            | Commands::Chat(ChatCommands::Join { .. })
            | Commands::Send { .. }
            | Commands::React { .. }
            | Commands::Typing { .. }
            | Commands::Receipt { .. }
            | Commands::Listen { .. }
    );

    if needs_identity {
        let (pubkey, was_generated) = config.ensure_identity()?;
        if was_generated {
            let pk = nostr::PublicKey::from_hex(&pubkey)?;
            let npub = nostr::ToBech32::to_bech32(&pk)?;
            eprintln!("Generated new identity: {}", npub);
        }
    }

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
            InviteCommands::Accept { invite_id, event } => {
                commands::invite::accept(&invite_id, &event, &config, &storage, output).await
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
        Commands::Send { target, message } => {
            commands::message::send(&target, &message, &config, &storage, output).await
        }
        Commands::React { target, message_id, emoji } => {
            commands::message::react(&target, &message_id, &emoji, &config, &storage, output).await
        }
        Commands::Typing { target } => {
            commands::message::typing(&target, &config, &storage, output).await
        }
        Commands::Receipt { target, receipt_type, message_ids } => {
            let ids: Vec<&str> = message_ids.iter().map(|s| s.as_str()).collect();
            commands::message::receipt(&target, &receipt_type, &ids, &config, &storage, output).await
        }
        Commands::Read { target, limit } => {
            commands::message::read(&target, limit, &storage, output).await
        }
        Commands::Contact(cmd) => match cmd {
            ContactCommands::Add { pubkey, name } => {
                commands::contact::add(&pubkey, &name, &storage, output).await
            }
            ContactCommands::List => {
                commands::contact::list(&storage, output).await
            }
            ContactCommands::Remove { name } => {
                commands::contact::remove(&name, &storage, output).await
            }
        },
        Commands::Listen { chat } => {
            commands::message::listen(chat.as_deref(), &config, &storage, output).await
        }
        Commands::Receive { event } => {
            commands::message::receive(&event, &storage, output).await
        }
    }
}
