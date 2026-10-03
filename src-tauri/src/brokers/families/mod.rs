//! Broker families: one generic implementation shared by every member
//! broker whose API is the same product under a different host (audit
//! Part D.1). A member broker directory holds only its configuration and
//! hook overrides.

pub mod noren;
pub mod xts;
