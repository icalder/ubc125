// Channel bank constants
/// Channels per bank (1–50)
pub const CHANNELS_PER_BANK: u32 = 50;

/// Maximum total channels (1–500)
pub const MAX_CHANNELS: u32 = 500;

/// Number of banks (1–10)
pub const NUM_BANKS: usize = 10;

/// Maximum level for volume/squelch (0–15)
pub const MAX_LEVEL: u8 = 15;

// Serial / polling constants
/// Serial port read timeout (ms)
pub const PORT_TIMEOUT_MS: u64 = 100;

/// Command response read timeout (ms)
pub const READ_TIMEOUT_MS: u64 = 500;

/// Scanner status poll interval (ms)
pub const POLL_INTERVAL_MS: u64 = 250;

/// Poll timeout when actively fetching channels (ms)
pub const POLL_INTERVAL_ACTIVE_MS: u64 = 1;

/// Poll timeout when idle (ms)
pub const POLL_INTERVAL_IDLE_MS: u64 = 50;

// Frequency constants
/// Number of digits in a raw frequency string (e.g. "01239750")
pub const FREQUENCY_DIGITS: usize = 8;

/// Number of MHz digits in user frequency input
pub const MHZ_DIGITS: usize = 4;

/// Number of KHz digits in user frequency input
pub const KHZ_DIGITS: usize = 4;

// GLG response column indices
/// Frequency field in GLG response
pub const GLG_FREQ_IDX: usize = 1;

/// Modulation field in GLG response
pub const GLG_MOD_IDX: usize = 2;

/// Channel name field in GLG response
pub const GLG_CHANNEL_NAME_IDX: usize = 7;

/// Squelch state field in GLG response (1 = open/detected).
/// Used as a proxy for signal detection: an open squelch indicates
/// a signal is being received.
pub const GLG_SQUELCH_STATE_IDX: usize = 8;

/// Channel index field in GLG response (used to derive bank)
pub const GLG_CHANNEL_INDEX_IDX: usize = 11;

/// Minimum fields required for a valid GLG response.
/// Covers indices 0 through GLG_CHANNEL_INDEX_IDX (11).
pub const GLG_MIN_FIELDS: usize = 12;

// CIN response column indices
/// Channel index field in CIN response
pub const CIN_INDEX_IDX: usize = 1;

/// Channel name field in CIN response
pub const CIN_NAME_IDX: usize = 2;

/// Frequency field in CIN response
pub const CIN_FREQ_IDX: usize = 3;

/// Modulation field in CIN response
pub const CIN_MOD_IDX: usize = 4;

/// Minimum fields required for a valid CIN response
pub const CIN_MIN_FIELDS: usize = 5;

// UI dimensions
/// Popup width for confirm-delete dialog (percent)
pub const POPUP_WIDTH_CONFIRM: u16 = 60;

/// Popup height for confirm-delete dialog (percent)
pub const POPUP_HEIGHT_CONFIRM: u16 = 20;

/// Popup width for set-level dialog (percent)
pub const POPUP_WIDTH_LEVEL: u16 = 40;

/// Popup height for set-level dialog (percent)
pub const POPUP_HEIGHT_LEVEL: u16 = 20;

/// Popup width for edit-channel dialog (percent)
pub const POPUP_WIDTH_EDIT: u16 = 60;

/// Popup height for edit-channel dialog (percent)
pub const POPUP_HEIGHT_EDIT: u16 = 40;

/// Table column width for channel index
pub const TABLE_COL_INDEX: u16 = 5;

/// Table column width for channel name
pub const TABLE_COL_NAME: u16 = 20;

/// Table column width for frequency
pub const TABLE_COL_FREQ: u16 = 10;

/// Table column width for modulation
pub const TABLE_COL_MOD: u16 = 5;
