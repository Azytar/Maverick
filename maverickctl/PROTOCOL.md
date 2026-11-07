# Maverick control protocol 1

The wire version is independent of package versions. Before state/query,
dispatch, reload, restart, quit or subscribe, the client sends `protocol\n`
on the same connection and requires exactly `protocol 1\n`. Another version,
a missing response or a legacy unknown-command reply fails clearly without
sending the requested control command. Ping/identify may be used without
negotiation solely for discovery of legacy running instances.

Protocol 1 preserves the existing ping, identify, state, query, dispatch,
subscribe, reload, restart and quit payload formats. `dispatch` acknowledges
queue acceptance rather than successful execution. A subscriber first receives
`ok subscribe`, followed by one event per line. Each wire frame must end in LF;
an optional preceding CR is tolerated. Payloads are UTF-8, limited to 65536
bytes before LF; reads are bounded before allocation and partial EOF is an
error. Commands reject embedded CR/LF before connecting. Server replies and
events preserve UTF-8 boundaries when applying their existing output limits.

Socket paths remain `$XDG_RUNTIME_DIR/maverick/<sid>/control.sock`, under private
0700 directories, with the existing owner-only socket and SO_PEERCRED boundary.
The server limits connection handlers and subscriptions; the client does not
circumvent those limits. CLI discovery must not delete records merely because
a live server has an incompatible control protocol.

The initial supported pairing is this client with Maverick's
`engineering/optional-control` implementation of protocol 1. Before completing the extraction, the Maverick
installer must pin an immutable published client commit and the external
X11/control campaign must run against that checkout. Remote publication and
that pin remain pending; the prepared client is currently built separately
from the development checkout. Legacy clients remain accepted by protocol-1
servers; any future incompatible wire version must explicitly retire legacy
commands or require a negotiated connection rather than silently changing
responses that old clients can interpret.
