use std::any::Any;
use std::fmt;
use std::sync::Arc;

pub(crate) struct RuntimeMemoryCallbackError(Box<dyn Any + Send + Sync + 'static>);

impl RuntimeMemoryCallbackError {
    pub(crate) fn new<T>(value: T) -> Self
    where
        T: Any + Send + Sync + 'static,
    {
        Self(Box::new(value))
    }

    pub(crate) fn downcast<T>(self) -> Result<T, Self>
    where
        T: Any + Send + Sync + 'static,
    {
        match self.0.downcast::<T>() {
            Ok(value) => Ok(*value),
            Err(value) => Err(Self(value)),
        }
    }
}

impl fmt::Debug for RuntimeMemoryCallbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeMemoryCallbackError")
            .finish_non_exhaustive()
    }
}

pub(crate) type RuntimeMemoryCallback = Arc<
    dyn Fn(
            &str,
            Option<&str>,
            Option<&str>,
            u32,
        ) -> Result<Option<String>, RuntimeMemoryCallbackError>
        + Send
        + Sync
        + 'static,
>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionMemoryOutputInvalidReason {
    EmptyOutput,
    JsonArrayMissing,
    NoValidEntries,
}

impl SessionMemoryOutputInvalidReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::EmptyOutput => "empty_output",
            Self::JsonArrayMissing => "json_array_missing",
            Self::NoValidEntries => "no_valid_entries",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionMemoryOutputDiagnostic {
    pub(crate) cycle_index: u32,
    pub(crate) backend: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reason: SessionMemoryOutputInvalidReason,
}

pub(crate) type SessionMemoryDiagnosticCallback =
    Arc<dyn Fn(&SessionMemoryOutputDiagnostic) + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub(crate) struct RuntimeMemoryCallbacks {
    pub(crate) session_memory: Option<RuntimeMemoryCallback>,
    pub(crate) memory_compaction: Option<RuntimeMemoryCallback>,
    pub(crate) session_memory_diagnostic: Option<SessionMemoryDiagnosticCallback>,
}

impl fmt::Debug for RuntimeMemoryCallbacks {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeMemoryCallbacks")
            .field(
                "session_memory",
                &self.session_memory.as_ref().map(|_| "<callback>"),
            )
            .field(
                "memory_compaction",
                &self.memory_compaction.as_ref().map(|_| "<callback>"),
            )
            .field(
                "session_memory_diagnostic",
                &self
                    .session_memory_diagnostic
                    .as_ref()
                    .map(|_| "<callback>"),
            )
            .finish()
    }
}
