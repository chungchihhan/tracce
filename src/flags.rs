//! User-editable flag list: glob patterns matched against command argv and
//! file paths, classified as `Warning` (yellow) or `Critical` (red) in the
//! TUI. Distinct from `sensitive.rs`, which is a fixed, non-configurable
//! list of secret-adjacent paths.
