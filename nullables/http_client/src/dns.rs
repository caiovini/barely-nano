use core::future::poll_fn;
use core::task::Poll;
use embedded_nal_async::{Dns, IpAddr, AddrType};
use smoltcp::socket::dns::{Socket as DnsSocket, GetQueryResultError};
use alloc::string::ToString;
use kernel_common::net::NET;

pub struct AsyncDns;

impl Dns for AsyncDns {
    type Error = anyhow::Error;

    async fn get_host_by_name(&self, name: &str, _addr_type: AddrType) -> Result<IpAddr, Self::Error> {
        // First start the query
        let query_name = name.to_string();
        
        let mut handle = None;
        if let Some(ref mut ctx) = *NET.lock() {
            let socket = ctx.sockets.get_mut::<DnsSocket>(ctx.dns_handle);
            match socket.start_query(ctx.iface.context(), &query_name, smoltcp::wire::DnsQueryType::A) {
                Ok(h) => { handle = Some(h); },
                Err(_) => return Err(anyhow::anyhow!("DNS query failed to start")),
            }
        }

        let query_handle = handle.unwrap();

        poll_fn(|cx| {
            if let Some(ref mut ctx) = *NET.lock() {
                let socket = ctx.sockets.get_mut::<DnsSocket>(ctx.dns_handle);
                match socket.get_query_result(query_handle) {
                    Ok(addrs) => {
                        for addr in addrs {
                            match addr {
                                smoltcp::wire::IpAddress::Ipv4(ipv4) => {
                                    let octets: [u8; 4] = unsafe { core::mem::transmute(ipv4) };
                                    return Poll::Ready(Ok(IpAddr::V4(
                                        embedded_nal_async::Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3])
                                    )));
                                }
                                smoltcp::wire::IpAddress::Ipv6(ipv6) => {
                                    let octets: [u8; 16] = unsafe { core::mem::transmute(ipv6) };
                                    return Poll::Ready(Ok(IpAddr::V6(
                                        embedded_nal_async::Ipv6Addr::from(octets)
                                    )));
                                }
                            }
                        }
                        Poll::Ready(Err(anyhow::anyhow!("No IP address found")))
                    },
                    Err(GetQueryResultError::Pending) => {
                        socket.register_query_waker(query_handle, cx.waker());
                        Poll::Pending
                    },
                    Err(_) => Poll::Ready(Err(anyhow::anyhow!("DNS resolution failed"))),
                }
            } else {
                Poll::Ready(Err(anyhow::anyhow!("No network context")))
            }
        }).await
    }

    async fn get_host_by_address(&self, _addr: IpAddr, _result: &mut [u8]) -> Result<usize, Self::Error> {
        Err(anyhow::anyhow!("Reverse DNS not implemented"))
    }
}
