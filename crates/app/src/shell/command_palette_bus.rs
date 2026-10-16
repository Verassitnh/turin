//! Port of upstream/apps/web/src/commandPaletteBus.ts.

use parking_lot::Mutex;
use std::sync::OnceLock;
use turin_contracts::environment::EnvironmentId;
use turin_contracts::pull_request::PullRequestLinkedThreadsEntry;

/// `CommandPaletteLinkedThreads`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandPaletteLinkedThreads {
    pub environment_id: EnvironmentId,
    pub threads: Vec<PullRequestLinkedThreadsEntry>,
}

/// `CommandPaletteOpenDetail`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandPaletteOpenDetail {
    pub open: Option<CommandPaletteOpenMode>,
    pub query: Option<String>,
    pub linked_threads: Option<CommandPaletteLinkedThreads>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandPaletteOpenMode {
    AddProject,
    NewThreadIn,
}

type Listener = Box<dyn Fn(CommandPaletteOpenDetail) + Send + Sync>;

struct Bus {
    listeners: Vec<Listener>,
    open: bool,
}

fn bus() -> &'static Mutex<Bus> {
    static BUS: OnceLock<Mutex<Bus>> = OnceLock::new();
    BUS.get_or_init(|| {
        Mutex::new(Bus {
            listeners: Vec::new(),
            open: false,
        })
    })
}

/// `openCommandPalette`.
pub fn open_command_palette(detail: Option<CommandPaletteOpenDetail>) {
    let detail = detail.unwrap_or_default();
    for listener in &bus().lock().listeners {
        listener(detail.clone());
    }
}

/// `onOpenCommandPalette`.
pub fn on_open_command_palette(
    listener: impl Fn(CommandPaletteOpenDetail) + Send + Sync + 'static,
) -> CommandPaletteSubscription {
    bus().lock().listeners.push(Box::new(listener));
    CommandPaletteSubscription
}

pub struct CommandPaletteSubscription;

/// GPUI host marks whether the palette is mounted.
pub fn set_command_palette_open(open: bool) {
    bus().lock().open = open;
}

/// `isCommandPaletteOpen`.
pub fn is_command_palette_open() -> bool {
    bus().lock().open
}
