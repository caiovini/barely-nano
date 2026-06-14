use smoltcp::socket::dns::{Socket, QueryHandle};
fn test(socket: &mut Socket, handle: QueryHandle) {
    socket.cancel_query(handle);
}
