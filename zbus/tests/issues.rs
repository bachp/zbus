use std::sync::{mpsc::channel, Arc, Condvar, Mutex};

use event_listener::Event;
use ntest::timeout;
use test_log::test;
use tracing::{instrument, trace};
use zbus::block_on;

use zbus_names::UniqueName;
use zvariant::{OwnedObjectPath, OwnedValue, Type};

use zbus::{
    blocking::{self, MessageIterator},
    message::Message,
    object_server::SignalContext,
    Connection, Result,
};

#[test]
#[timeout(15000)]
fn issue_68() {
    // Tests the fix for https://github.com/dbus2/zbus/issues/68
    //
    // While this is not an exact reproduction of the issue 68, the underlying problem it
    // produces is exactly the same: `Connection::call_method` dropping all incoming messages
    // while waiting for the reply to the method call.
    let conn = blocking::Connection::session().unwrap();
    let stream = MessageIterator::from(&conn);

    // Send a message as client before service starts to process messages
    let client_conn = blocking::Connection::session().unwrap();
    let destination = conn.unique_name().map(UniqueName::<'_>::from).unwrap();
    let msg = Message::method("/org/freedesktop/Issue68", "Ping")
        .unwrap()
        .destination(destination)
        .unwrap()
        .interface("org.freedesktop.Issue68")
        .unwrap()
        .build(&())
        .unwrap();
    let serial = msg.primary_header().serial_num();
    client_conn.send(&msg).unwrap();

    zbus::blocking::fdo::DBusProxy::new(&conn)
        .unwrap()
        .get_id()
        .unwrap();

    for m in stream {
        let msg = m.unwrap();

        if msg.primary_header().serial_num() == serial {
            break;
        }
    }
}

#[test]
#[timeout(15000)]
fn issue104() {
    // Tests the fix for https://github.com/dbus2/zbus/issues/104
    //
    // The issue is caused by `proxy` macro adding `()` around the return value of methods
    // with multiple out arguments, ending up with double parenthesis around the signature of
    // the return type and zbus only removing the outer `()` only and then it not matching the
    // signature we receive on the reply message.
    use zvariant::{ObjectPath, Value};

    struct Secret;
    #[zbus::interface(name = "org.freedesktop.Secret.Service")]
    impl Secret {
        fn open_session(
            &self,
            _algorithm: &str,
            input: Value<'_>,
        ) -> zbus::fdo::Result<(OwnedValue, OwnedObjectPath)> {
            Ok((
                OwnedValue::try_from(input).unwrap(),
                ObjectPath::try_from("/org/freedesktop/secrets/Blah")
                    .unwrap()
                    .into(),
            ))
        }
    }

    let secret = Secret;
    let conn = blocking::connection::Builder::session()
        .unwrap()
        .serve_at("/org/freedesktop/secrets", secret)
        .unwrap()
        .build()
        .unwrap();
    let service_name = conn.unique_name().unwrap().clone();

    {
        let conn = blocking::Connection::session().unwrap();
        #[zbus::proxy(
            interface = "org.freedesktop.Secret.Service",
            assume_defaults = true,
            gen_async = false
        )]
        trait Secret {
            fn open_session(
                &self,
                algorithm: &str,
                input: &zvariant::Value<'_>,
            ) -> zbus::Result<(OwnedValue, OwnedObjectPath)>;
        }

        let proxy = SecretProxy::builder(&conn)
            .destination(UniqueName::from(service_name))
            .unwrap()
            .path("/org/freedesktop/secrets")
            .unwrap()
            .build()
            .unwrap();

        trace!("Calling open_session");
        proxy.open_session("plain", &Value::from("")).unwrap();
        trace!("Called open_session");
    };
}

// This one we just want to see if it builds, no need to run it. For details see:
//
// https://github.com/dbus2/zbus/issues/121
#[test]
#[ignore]
fn issue_121() {
    use zbus::proxy;

    #[proxy(interface = "org.freedesktop.IBus", assume_defaults = true)]
    trait IBus {
        /// CurrentInputContext property
        #[zbus(property)]
        fn current_input_context(&self) -> zbus::Result<OwnedObjectPath>;

        /// Engines property
        #[zbus(property)]
        fn engines(&self) -> zbus::Result<Vec<zvariant::OwnedValue>>;
    }
}

#[test]
#[timeout(15000)]
fn issue_122() {
    let conn = blocking::Connection::session().unwrap();
    let stream = MessageIterator::from(&conn);

    #[allow(clippy::mutex_atomic)]
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let pair2 = Arc::clone(&pair);

    let child = std::thread::spawn(move || {
        {
            let (lock, cvar) = &*pair2;
            let mut started = lock.lock().unwrap();
            *started = true;
            cvar.notify_one();
        }

        for m in stream {
            let msg = m.unwrap();
            let hdr = msg.header();

            if hdr.member().map(|m| m.as_str()) == Some("ZBusIssue122") {
                break;
            }
        }
    });

    // Wait for the receiving thread to start up.
    let (lock, cvar) = &*pair;
    let mut started = lock.lock().unwrap();
    while !*started {
        started = cvar.wait(started).unwrap();
    }
    // Still give it some milliseconds to ensure it's already blocking on receive_message call
    // when we send a message.
    std::thread::sleep(std::time::Duration::from_millis(100));

    let destination = conn.unique_name().map(UniqueName::<'_>::from).unwrap();
    let msg = Message::method("/does/not/matter", "ZBusIssue122")
        .unwrap()
        .destination(destination)
        .unwrap()
        .build(&())
        .unwrap();
    conn.send(&msg).unwrap();

    child.join().unwrap();
}

#[test]
#[ignore]
fn issue_81() {
    use zbus::proxy;
    use zvariant::{OwnedValue, Type};

    #[derive(
        Debug, PartialEq, Eq, Clone, Type, OwnedValue, serde::Serialize, serde::Deserialize,
    )]
    pub struct DbusPath {
        id: String,
        path: OwnedObjectPath,
    }

    #[proxy(assume_defaults = true)]
    trait Session {
        #[zbus(property)]
        fn sessions_tuple(&self) -> zbus::Result<(String, String)>;

        #[zbus(property)]
        fn sessions_struct(&self) -> zbus::Result<DbusPath>;
    }
}

#[test]
#[timeout(15000)]
fn issue173() {
    // Tests the fix for https://github.com/dbus2/zbus/issues/173
    //
    // The issue is caused by proxy not keeping track of its destination's owner changes
    // (service restart) and failing to receive signals as a result.
    let (tx, rx) = channel();
    let child = std::thread::spawn(move || {
        let conn = blocking::Connection::session().unwrap();
        #[zbus::proxy(
            interface = "org.freedesktop.zbus.ComeAndGo",
            default_service = "org.freedesktop.zbus.ComeAndGo",
            default_path = "/org/freedesktop/zbus/ComeAndGo"
        )]
        trait ComeAndGo {
            #[zbus(signal)]
            fn the_signal(&self) -> zbus::Result<()>;
        }

        let proxy = ComeAndGoProxyBlocking::new(&conn).unwrap();
        let signals = proxy.receive_the_signal().unwrap();
        tx.send(()).unwrap();

        // We receive two signals, each time from different unique names. W/o the fix for
        // issue#173, the second iteration hangs.
        for _ in signals.take(2) {
            tx.send(()).unwrap();
        }
    });

    struct ComeAndGo;
    #[zbus::interface(name = "org.freedesktop.zbus.ComeAndGo")]
    impl ComeAndGo {
        #[zbus(signal)]
        async fn the_signal(signal_ctxt: &SignalContext<'_>) -> zbus::Result<()>;
    }

    rx.recv().unwrap();
    for _ in 0..2 {
        let conn = blocking::connection::Builder::session()
            .unwrap()
            .serve_at("/org/freedesktop/zbus/ComeAndGo", ComeAndGo)
            .unwrap()
            .name("org.freedesktop.zbus.ComeAndGo")
            .unwrap()
            .build()
            .unwrap();

        let iface_ref = conn
            .object_server()
            .interface::<_, ComeAndGo>("/org/freedesktop/zbus/ComeAndGo")
            .unwrap();
        block_on(ComeAndGo::the_signal(iface_ref.signal_context())).unwrap();

        rx.recv().unwrap();

        // Now we release the name ownership to use a different connection (i-e new unique
        // name).
        conn.release_name("org.freedesktop.zbus.ComeAndGo").unwrap();
    }

    child.join().unwrap();
}

#[test]
#[timeout(15000)]
fn issue_260() {
    // Low-level server example in the book doesn't work. The reason was that
    // `Connection::request_name` implicitly created the associated `ObjectServer` to avoid
    // #68. This meant that the `ObjectServer` ended up replying to the incoming method call
    // with an error, before the service code could do so.
    block_on(async {
        let connection = Connection::session().await?;

        connection.request_name("org.zbus.Issue260").await?;

        futures_util::try_join!(
            issue_260_service(&connection),
            issue_260_client(&connection),
        )?;

        Ok::<(), zbus::Error>(())
    })
    .unwrap();
}

async fn issue_260_service(connection: &Connection) -> Result<()> {
    use futures_util::stream::TryStreamExt;

    let mut stream = zbus::MessageStream::from(connection);
    while let Some(msg) = stream.try_next().await? {
        let msg_header = msg.header();

        match msg_header.message_type() {
            zbus::message::Type::MethodCall => {
                connection.reply(&msg, &()).await?;

                break;
            }
            _ => continue,
        }
    }

    Ok(())
}

async fn issue_260_client(connection: &Connection) -> Result<()> {
    zbus::Proxy::new(
        connection,
        "org.zbus.Issue260",
        "/org/zbus/Issue260",
        "org.zbus.Issue260",
    )
    .await?
    .call::<_, _, ()>("Whatever", &())
    .await?;
    Ok(())
}

#[test(tokio::test(flavor = "multi_thread", worker_threads = 2))]
// Issue specific to tokio runtime.
#[cfg(all(unix, feature = "tokio", feature = "p2p"))]
#[instrument]
async fn issue_279() {
    // On failure to read from the socket, we were closing the error channel from the sender
    // side and since the underlying tokio API doesn't provide a `close` method on the sender,
    // the async-channel abstraction was achieving this through calling `close` on receiver,
    // which is behind an async mutex and we end up with a deadlock.
    use futures_util::{stream::TryStreamExt, try_join};
    use tokio::net::UnixStream;
    use zbus::{connection::Builder, MessageStream};

    let guid = zbus::Guid::generate();
    let (p0, p1) = UnixStream::pair().unwrap();

    let server = Builder::unix_stream(p0).server(guid).unwrap().p2p().build();
    let client = Builder::unix_stream(p1).p2p().build();
    let (client, server) = try_join!(client, server).unwrap();
    let mut stream = MessageStream::from(client);
    let next_msg_fut = stream.try_next();

    drop(server);

    assert!(matches!(next_msg_fut.await, Err(_)));
}

#[test(tokio::test(flavor = "multi_thread"))]
// Issue specific to tokio runtime.
#[cfg(all(unix, feature = "tokio"))]
#[instrument]
async fn issue_310() {
    // The issue was we were deadlocking on fetching the new property value after invalidation.
    // This turned out to be caused by us trying to grab a read lock on resource while holding
    // a write lock. Thanks to connman for being weird and invalidating the property just before
    // updating it, so this issue could be exposed.
    use futures_util::StreamExt;
    use zbus::connection::Builder;

    struct Station(u64);

    #[zbus::interface(name = "net.connman.iwd.Station")]
    impl Station {
        #[zbus(property)]
        fn connected_network(&self) -> OwnedObjectPath {
            format!("/net/connman/iwd/0/33/Network/{}", self.0)
                .try_into()
                .unwrap()
        }
    }

    #[zbus::proxy(
        interface = "net.connman.iwd.Station",
        default_service = "net.connman.iwd"
    )]
    trait Station {
        #[zbus(property)]
        fn connected_network(&self) -> zbus::Result<OwnedObjectPath>;
    }
    let connection = Builder::session()
        .unwrap()
        .serve_at("/net/connman/iwd/0/33", Station(0))
        .unwrap()
        .name("net.connman.iwd")
        .unwrap()
        .build()
        .await
        .unwrap();
    let event = Arc::new(event_listener::Event::new());
    let conn_clone = connection.clone();
    let event_clone = event.clone();
    tokio::spawn(async move {
        for _ in 0..10 {
            let listener = event_clone.listen();
            let iface_ref = conn_clone
                .object_server()
                .interface::<_, Station>("/net/connman/iwd/0/33")
                .await
                .unwrap();

            {
                let iface = iface_ref.get().await;
                iface
                    .connected_network_invalidate(iface_ref.signal_context())
                    .await
                    .unwrap();
                iface
                    .connected_network_changed(iface_ref.signal_context())
                    .await
                    .unwrap();
            }
            listener.await;
            iface_ref.get_mut().await.0 += 1;
        }
    });

    let station = StationProxy::builder(&connection)
        .path("/net/connman/iwd/0/33")
        .unwrap()
        .build()
        .await
        .unwrap();

    let mut changes = station.receive_connected_network_changed().await;

    let mut last_received = 0;
    while last_received < 9 {
        let change = changes.next().await.unwrap();
        let path = change.get().await.unwrap();
        let received: u64 = path
            .split('/')
            .last()
            .unwrap()
            .parse()
            .expect("invalid path");
        assert!(received >= last_received);
        last_received = received;
        event.notify(1);
    }
}

#[test]
#[ignore]
fn issue_466() {
    #[zbus::proxy(interface = "org.Some.Thing1", assume_defaults = true)]
    trait MyGreeter {
        fn foo(
            &self,
            arg: &(u32, zbus::zvariant::Value<'_>),
        ) -> zbus::Result<(u32, zbus::zvariant::OwnedValue)>;

        #[zbus(property)]
        fn bar(&self) -> zbus::Result<(u32, zbus::zvariant::OwnedValue)>;
    }
}

#[instrument]
#[test]
fn concurrent_interface_methods() {
    // This is  test case for ensuring the regression of #799 doesn't come back.
    block_on(async {
        struct Iface(Event);

        #[zbus::interface(name = "org.zbus.test.issue799")]
        impl Iface {
            async fn method1(&self) {
                self.0.notify(1);
                // Never return
                std::future::pending::<()>().await;
            }

            async fn method2(&self) {}
        }

        let event = Event::new();
        let listener = event.listen();
        let iface = Iface(event);
        let conn = zbus::connection::Builder::session()
            .unwrap()
            .name("org.zbus.test.issue799")
            .unwrap()
            .serve_at("/org/zbus/test/issue799", iface)
            .unwrap()
            .build()
            .await
            .unwrap();

        #[zbus::proxy(
            default_service = "org.zbus.test.issue799",
            default_path = "/org/zbus/test/issue799",
            interface = "org.zbus.test.issue799"
        )]
        trait Iface {
            async fn method1(&self) -> Result<()>;
            async fn method2(&self) -> Result<()>;
        }

        let proxy = IfaceProxy::new(&conn).await.unwrap();
        let proxy_clone = proxy.clone();
        conn.executor()
            .spawn(
                async move {
                    proxy_clone.method1().await.unwrap();
                },
                "method1",
            )
            .detach();
        // Wait till the `method1`` is called.
        listener.await;

        // Now while the `method1` is in progress, a call to `method2` should just work.
        proxy.method2().await.unwrap();
    })
}

#[cfg(all(unix, feature = "p2p"))]
#[instrument]
#[test]
#[timeout(15000)]
fn issue_813() {
    // Our server-side handshake code was unable to handle FDs being sent in the first messages
    // if the client sent them too quickly after sending `BEGIN` command.
    //
    // We test this by manually sending out the auth commands together with 2 method calls with
    // 1 FD each. Before a fix for this issue, the server handshake would fail with an
    // `Unexpected FDs during handshake` error.
    use futures_util::try_join;
    use nix::unistd::Uid;
    #[cfg(not(feature = "tokio"))]
    use std::os::unix::net::UnixStream;
    use std::{os::fd::AsFd, vec};
    #[cfg(feature = "tokio")]
    use tokio::net::UnixStream;
    use zbus::{conn::socket::WriteHalf, connection::Builder};
    use zvariant::Fd;

    #[derive(Debug)]
    struct Issue813Iface {
        event: event_listener::Event,
        call_count: u8,
    }
    #[zbus::interface(interface = "org.zbus.Issue813")]
    impl Issue813Iface {
        #[instrument]
        fn pass_fd(&mut self, fd: Fd<'_>) {
            self.call_count += 1;
            tracing::debug!("`PassFd` called with {} {} times", fd, self.call_count);
            if self.call_count == 2 {
                self.event.notify(1);
            }
        }
    }
    #[zbus::proxy(
        gen_blocking = false,
        default_path = "/org/zbus/Issue813",
        interface = "org.zbus.Issue813"
    )]
    trait Issue813 {
        fn pass_fd(&self, fd: Fd<'_>) -> zbus::Result<()>;
    }

    block_on(async move {
        let guid = zbus::Guid::generate();
        let (p0, p1) = UnixStream::pair().unwrap();

        let client_event = event_listener::Event::new();
        let client_listener = client_event.listen();
        let server_event = event_listener::Event::new();
        let server_listener = server_event.listen();
        let server = async move {
            let _conn = Builder::unix_stream(p0)
                .server(guid)?
                .p2p()
                .serve_at(
                    "/org/zbus/Issue813",
                    Issue813Iface {
                        event: server_event,
                        call_count: 0,
                    },
                )?
                .name("org.zbus.Issue813")?
                .build()
                .await?;
            client_listener.await;

            Result::<()>::Ok(())
        };
        let client = async move {
            let commands = format!(
                "\0AUTH EXTERNAL {}\r\nNEGOTIATE_UNIX_FD\r\nBEGIN\r\n",
                hex::encode(Uid::effective().to_string())
            );
            let mut bytes: Vec<u8> = commands.bytes().collect();
            let fd = std::io::stdin();
            let msg = zbus::message::Message::method("/org/zbus/Issue813", "PassFd")?
                .destination("org.zbus.Issue813")?
                .interface("org.zbus.Issue813")?
                .build(&(Fd::from(fd.as_fd())))?;
            let msg_data = msg.data();
            let mut fds = vec![];
            for _ in 0..2 {
                bytes.extend_from_slice(&*msg_data);
                fds.push(fd.as_fd());
            }

            #[cfg(feature = "tokio")]
            let mut split = zbus::conn::Socket::split(p1);
            #[cfg(not(feature = "tokio"))]
            let mut split = zbus::conn::Socket::split(async_io::Async::new(p1)?);
            split.write_mut().sendmsg(&bytes, &fds).await?;

            server_listener.await;
            client_event.notify(1);

            Ok(())
        };
        let (_, _) = try_join!(client, server)?;

        Result::<()>::Ok(())
    })
    .unwrap();
}
