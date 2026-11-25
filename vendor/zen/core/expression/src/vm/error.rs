use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum VMError {
    #[error("Opcode {opcode}: {message}")]
    OpcodeErr { opcode: String, message: String },

    #[error("Opcode out of bounds")]
    OpcodeOutOfBounds { index: usize, bytecode: String },

    #[error("Stack out of bounds")]
    StackOutOfBounds { stack: String },

    #[error("Failed to parse date time")]
    ParseDateTimeErr { timestamp: String },

    #[error("Number conversion error")]
    NumberConversionError,

    /// Norn `metering`: the isolate's [`crate::meter::Meter`] ran out.
    #[error("{0}")]
    BudgetExhausted(crate::meter::BudgetExhausted),

    /// Norn (#1711): an integer interval with more elements than the VM materializes
    /// (or whose allocation fails) is refused before any element is allocated.
    #[error("Interval {interval} has too many elements to enumerate")]
    IntervalTooLarge { interval: String },
}

pub(crate) type VMResult<T> = Result<T, VMError>;
