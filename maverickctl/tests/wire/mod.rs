//! Version negotiation for fake instances; command-specific replies stay in each fixture.
use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub fn read_command(stream: &mut UnixStream) -> io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut command = maverick_protocol::protocol::read_frame(&mut reader)?;
    if command.as_deref() == Some("protocol") {
        writeln!(
            stream,
            "protocol {}",
            maverick_protocol::protocol::PROTOCOL_VERSION
        )?;
        command = maverick_protocol::protocol::read_frame(&mut reader)?;
    }
    command
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "fixture received no command"))
}
