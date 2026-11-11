//! How the commands talk to whoever runs them, so that the same commands can have a command line now and a desktop
//! window later.

use anyhow::Result;

/// What a command needs from whoever runs it.
pub trait Ui {
    /// Says what is happening, e.g. "Fetching bruce-db".
    fn info(&mut self, message: &str);

    /// Shows what a command will do, as a list of steps, and asks once whether to go ahead, which is no unless they
    /// say yes.
    fn confirm(&mut self, title: &str, steps: &[String]) -> Result<bool>;
}
