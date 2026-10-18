//! A feature remembers who added it (`author`: the signed-in user's name where designs are
//! shared on a server; the timeline shows it). Older designs need no transformation: a feature
//! without `author` is by nobody in particular; the last release asks for an update instead of
//! opening a design with the newer format number.
