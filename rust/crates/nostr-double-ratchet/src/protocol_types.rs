use crate::UnixSeconds;
use rand::{CryptoRng, RngCore};

pub const MAX_SKIP: usize = 1000;

pub struct ProtocolContext<'a, R>
where
    R: RngCore + CryptoRng,
{
    pub now: UnixSeconds,
    pub rng: &'a mut R,
}

impl<'a, R> ProtocolContext<'a, R>
where
    R: RngCore + CryptoRng,
{
    pub fn new(now: UnixSeconds, rng: &'a mut R) -> Self {
        Self { now, rng }
    }
}
