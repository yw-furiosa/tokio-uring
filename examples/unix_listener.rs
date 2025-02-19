use std::env;

use tokio_uring::{net::UnixListener, Buffer, Submit};

fn main() {
    let args: Vec<_> = env::args().collect();

    if args.len() <= 1 {
        panic!("no addr specified");
    }

    let socket_addr: String = args[1].clone();

    tokio_uring::start(async {
        let listener = UnixListener::bind(&socket_addr).unwrap();

        loop {
            let stream = listener.accept().await.unwrap();
            let socket_addr = socket_addr.clone();
            tokio_uring::spawn(async move {
                let buf = Buffer::new(vec![1u8; 128]);

                let (n, buf) = stream.write(buf).submit().await.unwrap();
                println!("written to {}: {}", &socket_addr, n);

                let (read, buf) = stream.read(buf).await.unwrap();
                println!("read from {}: {:?}", &socket_addr, &buf[0][..read]);
            });
        }
    });
}
