//! Byte-for-byte splicing between two duplex streams.

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// Copies both directions until each side ends, propagating half-close: an EOF
/// on one reader shuts down the matching writer, so `push`'s "the file ended
/// here" and a peer's FIN survive the hop instead of turning into a hang.
pub async fn splice<AR, AW, BR, BW>(mut ar: AR, mut aw: AW, mut br: BR, mut bw: BW)
where
    AR: AsyncRead + Unpin,
    AW: AsyncWrite + Unpin,
    BR: AsyncRead + Unpin,
    BW: AsyncWrite + Unpin,
{
    let a_to_b = async {
        let _ = tokio::io::copy(&mut ar, &mut bw).await;
        let _ = bw.shutdown().await;
    };
    let b_to_a = async {
        let _ = tokio::io::copy(&mut br, &mut aw).await;
        let _ = aw.shutdown().await;
    };
    tokio::join!(a_to_b, b_to_a);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn splices_both_directions_and_propagates_eof() {
        let (a, a_peer) = tokio::io::duplex(64);
        let (b, b_peer) = tokio::io::duplex(64);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        tokio::spawn(splice(ar, aw, br, bw));

        let (mut a_r, mut a_w) = tokio::io::split(a_peer);
        let (mut b_r, mut b_w) = tokio::io::split(b_peer);
        a_w.write_all(b"ping").await.unwrap();
        a_w.shutdown().await.unwrap();
        let mut got = Vec::new();
        b_r.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"ping");

        b_w.write_all(b"pong").await.unwrap();
        b_w.shutdown().await.unwrap();
        let mut got = Vec::new();
        a_r.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, b"pong");
    }
}
