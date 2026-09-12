use std::process::{Child, Command, ExitStatus};

/// An OS child together with its per-command process-tree completion evidence.
///
/// The Linux supervisor exits with the original command's exit/signal status
/// only after every descendant has exited. No PID-indexed global registry is
/// used: ownership of the control channel moves with this child.
#[derive(Debug)]
pub struct ManagedChild {
    pub(super) process: Child,
    #[cfg(target_os = "linux")]
    pub(super) tree: Option<super::supervisor::ProcessTree>,
}

impl From<Child> for ManagedChild {
    fn from(process: Child) -> Self {
        Self {
            process,
            #[cfg(target_os = "linux")]
            tree: None,
        }
    }
}

impl ManagedChild {
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<Self> {
        #[cfg(target_os = "linux")]
        let tree = Some(super::supervisor::ProcessTree::prepare(command)?);
        let process = command.spawn()?;
        Ok(Self {
            process,
            #[cfg(target_os = "linux")]
            tree,
        })
    }

    pub fn id(&self) -> u32 {
        self.process.id()
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.process.try_wait()
    }

    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.process.wait()
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(tree) = self.tree.as_mut() {
            return tree.request_stop(true);
        }
        self.process.kill()
    }
}
