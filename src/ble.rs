use anyhow::{bail, Context, Result};
use bluer::{
    adv::Advertisement,
    gatt::{
        local::{
            characteristic_control, Application, Characteristic, CharacteristicControlEvent,
            CharacteristicNotify, CharacteristicNotifyMethod, CharacteristicWrite,
            CharacteristicWriteMethod, Service,
        },
        remote::Characteristic as RemoteCharacteristic,
        CharacteristicReader, CharacteristicWriter,
    },
    Adapter, AdapterEvent, Device, Uuid,
};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Nonce,
};
use futures_lite::StreamExt;
use iroh::{EndpointId, SecretKey, Signature};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet, os::unix::fs::OpenOptionsExt, path::Path, sync::Arc, time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

const SERVICE_UUID: Uuid = Uuid::from_u128(0xe6d0d000_7d7a_4c6f_8d8a_7c4547444f44);
const CHARACTERISTIC_UUID: Uuid = Uuid::from_u128(0xe6d0d001_7d7a_4c6f_8d8a_7c4547444f44);
const SERVER_LABEL: &[u8] = b"egdod-ble-server-v1";
const CLIENT_LABEL: &[u8] = b"egdod-ble-client-v1";
const KEY_LABEL: &[u8] = b"egdod-ble-key-v1";
const START: u8 = 1;
const MAX_FRAME: usize = 64 * 1024;
const SCAN_WAIT: Duration = Duration::from_secs(60);
const EXCHANGE_WAIT: Duration = Duration::from_secs(30);
const CHANNEL_WAIT: Duration = Duration::from_secs(3);

struct ServerHello {
    agent: EndpointId,
    ephemeral: [u8; 32],
    signature: Signature,
}

fn identity_message(
    label: &[u8],
    controller: &EndpointId,
    agent: &EndpointId,
    keys: &[[u8; 32]],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(label.len() + 64 + keys.len() * 32);
    out.extend_from_slice(label);
    out.extend_from_slice(controller.as_bytes());
    out.extend_from_slice(agent.as_bytes());
    for key in keys {
        out.extend_from_slice(key);
    }
    out
}

fn server_hello(key: &SecretKey, controller: &EndpointId, ephemeral: [u8; 32]) -> Vec<u8> {
    let agent = key.public();
    let signature = key.sign(&identity_message(
        SERVER_LABEL,
        controller,
        &agent,
        &[ephemeral],
    ));
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(agent.as_bytes());
    out.extend_from_slice(&ephemeral);
    out.extend_from_slice(&signature.to_bytes());
    out
}

fn parse_server_hello(bytes: &[u8]) -> Result<ServerHello> {
    if bytes.len() != 128 {
        bail!("BLE server hello is {} bytes, expected 128", bytes.len());
    }
    let agent =
        EndpointId::from_bytes(bytes[0..32].try_into().unwrap()).context("BLE agent node id")?;
    let ephemeral = bytes[32..64].try_into().unwrap();
    let signature = Signature::from_bytes(bytes[64..128].try_into().unwrap());
    Ok(ServerHello {
        agent,
        ephemeral,
        signature,
    })
}

fn verify_server_hello(
    bytes: &[u8],
    controller: &EndpointId,
    expected_agent: &EndpointId,
) -> Result<ServerHello> {
    let hello = parse_server_hello(bytes)?;
    if hello.agent != *expected_agent {
        bail!("BLE target node id does not match the requested agent");
    }
    expected_agent
        .verify(
            &identity_message(
                SERVER_LABEL,
                controller,
                expected_agent,
                &[hello.ephemeral],
            ),
            &hello.signature,
        )
        .context("verifying BLE target identity")?;
    Ok(hello)
}

fn verify_client_hello(
    bytes: &[u8],
    controller: &EndpointId,
    agent: &EndpointId,
    server_ephemeral: [u8; 32],
) -> Result<[u8; 32]> {
    if bytes.len() < 128 + 16 {
        bail!("BLE credential request is too short");
    }
    let claimed_controller = EndpointId::from_bytes(bytes[0..32].try_into().unwrap())
        .context("BLE controller node id")?;
    if claimed_controller != *controller {
        bail!("BLE write came from a controller other than the one baked into the image");
    }
    let client_ephemeral = bytes[32..64].try_into().unwrap();
    let signature = Signature::from_bytes(bytes[64..128].try_into().unwrap());
    controller
        .verify(
            &identity_message(
                CLIENT_LABEL,
                controller,
                agent,
                &[server_ephemeral, client_ephemeral],
            ),
            &signature,
        )
        .context("verifying BLE controller identity")?;
    Ok(client_ephemeral)
}

fn session_key(shared: [u8; 32], transcript: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(KEY_LABEL)
        .chain_update(shared)
        .chain_update(transcript)
        .finalize()
        .into()
}

fn nonce(n: u8) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[11] = n;
    *Nonce::from_slice(&bytes)
}

fn crypt(key: &[u8; 32], n: u8, aad: &[u8], clear: &[u8]) -> Result<Vec<u8>> {
    ChaCha20Poly1305::new(key.into())
        .encrypt(&nonce(n), Payload { msg: clear, aad })
        .map_err(|_| anyhow::anyhow!("BLE encryption failed"))
}

fn decrypt(key: &[u8; 32], n: u8, aad: &[u8], cipher: &[u8]) -> Result<Vec<u8>> {
    ChaCha20Poly1305::new(key.into())
        .decrypt(&nonce(n), Payload { msg: cipher, aad })
        .map_err(|_| anyhow::anyhow!("BLE credential authentication failed"))
}

fn network_conf(ssid: &str, psk: &str) -> Result<String> {
    if ssid.is_empty()
        || ssid.len() > 32
        || ssid.contains(['\n', '\r', '\0', '"', '\\'])
    {
        bail!("SSID must be 1–32 bytes without a quote, backslash, line break or NUL");
    }
    let raw = psk.len() == 64 && psk.bytes().all(|b| b.is_ascii_hexdigit());
    let psk_ok = (8..=63).contains(&psk.len()) || raw;
    if !psk_ok || psk.contains(['\n', '\r', '\0', '"', '\\']) {
        bail!("PSK must be 8–63 bytes or 64 hexadecimal digits without a quote, backslash, line break or NUL");
    }
    let ssid = ssid
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let psk_line = if raw {
        format!("psk={psk}")
    } else {
        format!("psk=\"{psk}\"")
    };
    Ok(format!(
        "network={{\n\tssid={ssid}\n\t{psk_line}\n\tkey_mgmt=WPA-PSK\n}}\n"
    ))
}

fn install(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".ble.tmp.{}", std::process::id()));
    let tmp = std::path::PathBuf::from(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    let result = (|| {
        file.write_all(bytes).context("writing BLE credentials")?;
        file.sync_all().context("flushing BLE credentials")?;
        drop(file);
        std::fs::rename(&tmp, path).with_context(|| format!("installing {}", path.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn frame_size(size: usize) -> Result<usize> {
    if size > MAX_FRAME {
        bail!("BLE frame exceeds {MAX_FRAME} bytes");
    }
    Ok(size)
}

async fn write_frame(writer: &mut CharacteristicWriter, bytes: &[u8]) -> Result<()> {
    frame_size(bytes.len())?;
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_frame(reader: &mut CharacteristicReader) -> Result<Vec<u8>> {
    let mut size = [0u8; 4];
    reader.read_exact(&mut size).await?;
    let size = u32::from_be_bytes(size) as usize;
    frame_size(size).with_context(|| format!("BLE frame declares {size} bytes"))?;
    let mut out = vec![0u8; size];
    reader.read_exact(&mut out).await?;
    Ok(out)
}

async fn adapter(name: Option<&str>) -> Result<(Adapter, bool)> {
    let session = bluer::Session::new()
        .await
        .context("connecting to bluetoothd")?;
    let adapter = match name {
        Some(name) => session
            .adapter(name)
            .with_context(|| format!("opening Bluetooth adapter {name}"))?,
        None => session
            .default_adapter()
            .await
            .context("finding a Bluetooth adapter")?,
    };
    let powered = adapter.is_powered().await?;
    if !powered {
        adapter.set_powered(true).await?;
    }
    Ok((adapter, powered))
}

pub fn local_name(id: &EndpointId) -> String {
    format!("egdod-{}", &id.to_string()[..8])
}

fn advertisement(id: &EndpointId) -> Advertisement {
    Advertisement {
        service_uuids: [SERVICE_UUID].into_iter().collect(),
        local_name: Some(local_name(id)),
        discoverable: Some(true),
        ..Default::default()
    }
}

pub async fn serve(controller: EndpointId, key_file: &Path, output: &Path) -> Result<()> {
    let key = crate::state::load_or_create_key(key_file, crate::state::OnUnusable::Replace)?;
    let agent = key.public();
    let (adapter, was_powered) = adapter(None).await?;
    let active = Arc::new(tokio::sync::Mutex::new(None));
    let result = tokio::select! {
        result = serve_on_adapter(&adapter, controller, key, agent, output, &active) => result,
        result = termination() => result.and_then(|()| Err(anyhow::anyhow!("BLE provisioning interrupted"))),
    };
    if let Some(peer) = active.lock().await.take() {
        let _ = adapter.remove_device(peer).await;
    }
    if !was_powered {
        let _ = adapter.set_powered(false).await;
    }
    result
}

async fn termination() -> Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("waiting for interrupt"),
        _ = terminate.recv() => Ok(()),
    }
}

async fn serve_on_adapter(
    adapter: &Adapter,
    controller: EndpointId,
    key: SecretKey,
    agent: EndpointId,
    output: &Path,
    active: &tokio::sync::Mutex<Option<bluer::Address>>,
) -> Result<()> {
    let adv = adapter.advertise(advertisement(&agent)).await?;
    let (mut control, handle) = characteristic_control();
    let app = Application {
        services: vec![Service {
            uuid: SERVICE_UUID,
            primary: true,
            characteristics: vec![Characteristic {
                uuid: CHARACTERISTIC_UUID,
                write: Some(CharacteristicWrite {
                    write: true,
                    write_without_response: true,
                    method: CharacteristicWriteMethod::Io,
                    ..Default::default()
                }),
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: handle,
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let app = adapter.serve_gatt_application(app).await?;
    eprintln!(
        "egdod: BLE first hop {} advertising {}",
        agent,
        local_name(&agent)
    );
    loop {
        let (mut reader, mut writer, peer) = match control.next().await {
            Some(CharacteristicControlEvent::Write(request)) => {
                let reader = request.accept()?;
                let peer = reader.device_address();
                (Some(reader), None, peer)
            }
            Some(CharacteristicControlEvent::Notify(writer)) => {
                let peer = writer.device_address();
                (None, Some(writer), peer)
            }
            None => bail!("BLE GATT service stopped"),
        };
        *active.lock().await = Some(peer);
        let paired = tokio::time::timeout(CHANNEL_WAIT, async {
            while reader.is_none() || writer.is_none() {
                match control.next().await {
                    Some(CharacteristicControlEvent::Write(request)) => {
                        let next = request.accept()?;
                        let address = next.device_address();
                        if peer != address {
                            continue;
                        }
                        reader = Some(next);
                    }
                    Some(CharacteristicControlEvent::Notify(next)) => {
                        let address = next.device_address();
                        if peer != address {
                            continue;
                        }
                        writer = Some(next);
                    }
                    None => bail!("BLE GATT service stopped"),
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;
        if let Err(error) = paired.context("BLE characteristic pairing timed out").and_then(|r| r) {
            let _ = adapter.remove_device(peer).await;
            *active.lock().await = None;
            eprintln!("egdod: refused BLE provisioning attempt: {error:#}");
            continue;
        }
        let mut installed = false;
        let attempt = tokio::time::timeout(EXCHANGE_WAIT, async {
            let mut reader = reader.unwrap();
            let mut writer = writer.unwrap();
            let mut start = [0];
            reader.read_exact(&mut start).await?;
            if start[0] != START {
                bail!("BLE provisioning start marker is invalid");
            }
            let mut random = [0u8; 32];
            getrandom::getrandom(&mut random).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
            let secret = StaticSecret::from(random);
            let server_ephemeral = XPublicKey::from(&secret).to_bytes();
            let hello = server_hello(&key, &controller, server_ephemeral);
            write_frame(&mut writer, &hello).await?;
            let request = read_frame(&mut reader).await?;
            let client_ephemeral =
                verify_client_hello(&request, &controller, &agent, server_ephemeral)?;
            let mut transcript = hello;
            transcript.extend_from_slice(&request[..128]);
            let shared = secret
                .diffie_hellman(&XPublicKey::from(client_ephemeral))
                .to_bytes();
            let session_key = session_key(shared, &transcript);
            let conf = decrypt(&session_key, 0, &transcript, &request[128..])?;
            if conf.is_empty() {
                bail!("BLE credential file is empty");
            }
            install(output, &conf)?;
            installed = true;
            let ack = crypt(&session_key, 1, &transcript, b"ok")?;
            if let Err(error) = write_frame(&mut writer, &ack).await {
                eprintln!("egdod: credentials installed but BLE acknowledgement failed: {error:#}");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok::<(), anyhow::Error>(())
        })
        .await;
        let _ = adapter.remove_device(peer).await;
        *active.lock().await = None;
        match attempt {
            Ok(Ok(())) => break,
            Ok(Err(error)) => eprintln!("egdod: refused BLE provisioning attempt: {error:#}"),
            Err(_) if installed => break,
            Err(_) => eprintln!("egdod: BLE provisioning attempt timed out"),
        }
    }
    eprintln!("egdod: network credentials received over BLE");
    drop(app);
    drop(adv);
    Ok(())
}

async fn characteristic(device: &Device) -> Result<RemoteCharacteristic> {
    for _ in 0..20 {
        for service in device.services().await? {
            if service.uuid().await? != SERVICE_UUID {
                continue;
            }
            for characteristic in service.characteristics().await? {
                if characteristic.uuid().await? == CHARACTERISTIC_UUID {
                    return Ok(characteristic);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    bail!("egdod BLE characteristic did not appear")
}

pub async fn provision(
    controller_key: SecretKey,
    expected_agent: EndpointId,
    ssid: String,
    psk: String,
    adapter_name: Option<&str>,
) -> Result<()> {
    let conf = network_conf(&ssid, &psk)?;
    let expected_name = local_name(&expected_agent);
    let (adapter, was_powered) = adapter(adapter_name).await?;
    let active = Arc::new(tokio::sync::Mutex::new(None));
    let active_during_provisioning = active.clone();
    let provisioning = async {
        let known_devices = adapter.device_addresses().await?;
        let mut events = adapter.discover_devices_with_changes().await?;
        let deadline = tokio::time::Instant::now() + SCAN_WAIT;
        let mut rejected = HashSet::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                bail!("waiting for the requested BLE target timed out");
            }
            let event = tokio::time::timeout(remaining, events.next())
                .await
                .context("waiting for the requested BLE target")?
                .context("Bluetooth discovery ended before the requested target appeared")?;
            let AdapterEvent::DeviceAdded(address) = event else {
                continue;
            };
            if rejected.contains(&address) {
                continue;
            }
            let device = adapter.device(address)?;
            if device.name().await?.as_deref() == Some(expected_name.as_str())
                && device.uuids().await?.unwrap_or_default().contains(&SERVICE_UUID)
            {
                rejected.insert(address);
                let was_known = known_devices.contains(&device.address());
                *active_during_provisioning.lock().await = Some((device.clone(), was_known));
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    bail!("waiting for the requested BLE target timed out");
                }
                let attempt = tokio::time::timeout(EXCHANGE_WAIT.min(remaining), async {
                    device.connect().await.context("connecting to BLE target")?;
                    let characteristic = characteristic(&device).await?;
                    let mut notify = characteristic.notify_io().await?;
                    let mut write = characteristic.write_io().await?;
                    write.write_all(&[START]).await?;
                    write.flush().await?;
                    let hello_bytes = read_frame(&mut notify).await?;
                    let controller = controller_key.public();
                    let hello =
                        verify_server_hello(&hello_bytes, &controller, &expected_agent)?;
                    let mut random = [0u8; 32];
                    getrandom::getrandom(&mut random)
                        .map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
                    let secret = StaticSecret::from(random);
                    let client_ephemeral = XPublicKey::from(&secret).to_bytes();
                    let signature = controller_key.sign(&identity_message(
                        CLIENT_LABEL,
                        &controller,
                        &expected_agent,
                        &[hello.ephemeral, client_ephemeral],
                    ));
                    let mut request = Vec::with_capacity(128);
                    request.extend_from_slice(controller.as_bytes());
                    request.extend_from_slice(&client_ephemeral);
                    request.extend_from_slice(&signature.to_bytes());
                    let mut transcript = hello_bytes;
                    transcript.extend_from_slice(&request);
                    let shared = secret
                        .diffie_hellman(&XPublicKey::from(hello.ephemeral))
                        .to_bytes();
                    let session_key = session_key(shared, &transcript);
                    request.extend_from_slice(&crypt(
                        &session_key,
                        0,
                        &transcript,
                        conf.as_bytes(),
                    )?);
                    write_frame(&mut write, &request).await?;
                    let ack = read_frame(&mut notify).await?;
                    if decrypt(&session_key, 1, &transcript, &ack)? != b"ok" {
                        bail!("BLE target returned an invalid acknowledgement");
                    }
                    eprintln!("egdod: {expected_agent} accepted network credentials over BLE; waiting for its normal IP dial");
                    Ok(())
                })
                .await
                .context("BLE provisioning exchange timed out")
                .and_then(|result| result);
                let _ = device.disconnect().await;
                if !was_known {
                    let _ = adapter.remove_device(device.address()).await;
                }
                *active_during_provisioning.lock().await = None;
                match attempt {
                    Ok(()) => return Ok::<(), anyhow::Error>(()),
                    Err(error) => eprintln!("egdod: rejected BLE candidate {address}: {error:#}"),
                }
            }
        }
    };
    let result = tokio::select! {
        result = provisioning => result,
        result = termination() => result.and_then(|()| Err(anyhow::anyhow!("BLE provisioning interrupted"))),
    };
    if let Some((device, was_known)) = active.lock().await.take() {
        let _ = device.disconnect().await;
        if !was_known {
            let _ = adapter.remove_device(device.address()).await;
        }
    }
    if !was_powered {
        let _ = adapter.set_powered(false).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_proofs_bind_both_nodes_and_ephemeral_keys() {
        let controller = SecretKey::from_bytes(&[1; 32]);
        let agent = SecretKey::from_bytes(&[2; 32]);
        let server_key = [3; 32];
        let client_key = [4; 32];
        let message = identity_message(
            CLIENT_LABEL,
            &controller.public(),
            &agent.public(),
            &[server_key, client_key],
        );
        let signature = controller.sign(&message);
        controller.public().verify(&message, &signature).unwrap();
        assert_eq!(message.len(), CLIENT_LABEL.len() + 128);
        assert_eq!(&message[..CLIENT_LABEL.len()], CLIENT_LABEL);
        assert_eq!(
            &message[CLIENT_LABEL.len()..CLIENT_LABEL.len() + 32],
            controller.public().as_bytes()
        );
        assert_eq!(
            &message[CLIENT_LABEL.len() + 32..CLIENT_LABEL.len() + 64],
            agent.public().as_bytes()
        );
        assert_eq!(
            &message[CLIENT_LABEL.len() + 64..CLIENT_LABEL.len() + 96],
            &server_key
        );
        assert_eq!(&message[CLIENT_LABEL.len() + 96..], &client_key);
        for offset in [
            0,
            CLIENT_LABEL.len(),
            CLIENT_LABEL.len() + 32,
            CLIENT_LABEL.len() + 64,
            CLIENT_LABEL.len() + 96,
        ] {
            let mut changed = message.clone();
            changed[offset] ^= 1;
            assert!(controller.public().verify(&changed, &signature).is_err());
        }
    }

    #[test]
    fn verification_rejects_wrong_identity_proofs() {
        let controller = SecretKey::from_bytes(&[1; 32]);
        let agent = SecretKey::from_bytes(&[2; 32]);
        let stranger = SecretKey::from_bytes(&[3; 32]);
        let server_ephemeral = [4; 32];
        let good_server = server_hello(&agent, &controller.public(), server_ephemeral);
        verify_server_hello(&good_server, &controller.public(), &agent.public()).unwrap();
        let mut bad_signature = good_server.clone();
        bad_signature[64] ^= 1;
        assert!(
            verify_server_hello(&bad_signature, &controller.public(), &agent.public()).is_err()
        );
        let bad_server = server_hello(&stranger, &controller.public(), server_ephemeral);
        assert!(verify_server_hello(&bad_server, &controller.public(), &agent.public()).is_err());

        let client_ephemeral = [5; 32];
        let message = identity_message(
            CLIENT_LABEL,
            &controller.public(),
            &agent.public(),
            &[server_ephemeral, client_ephemeral],
        );
        let mut request = Vec::new();
        request.extend_from_slice(controller.public().as_bytes());
        request.extend_from_slice(&client_ephemeral);
        request.extend_from_slice(&controller.sign(&message).to_bytes());
        request.extend_from_slice(&[0; 16]);
        assert_eq!(
            verify_client_hello(
                &request,
                &controller.public(),
                &agent.public(),
                server_ephemeral
            )
            .unwrap(),
            client_ephemeral
        );
        request[64..128].copy_from_slice(&stranger.sign(&message).to_bytes());
        assert!(verify_client_hello(
            &request,
            &controller.public(),
            &agent.public(),
            server_ephemeral
        )
        .is_err());
    }

    #[test]
    fn key_and_frame_boundaries_are_domain_separated() {
        let expected: [u8; 32] = Sha256::new()
            .chain_update(KEY_LABEL)
            .chain_update([7; 32])
            .chain_update(b"transcript")
            .finalize()
            .into();
        assert_eq!(session_key([7; 32], b"transcript"), expected);
        assert_ne!(session_key([7; 32], b"a"), session_key([7; 32], b"b"));
        assert_ne!(session_key([7; 32], b"a"), session_key([8; 32], b"a"));
        assert_ne!(nonce(0), nonce(1));
        assert_eq!(MAX_FRAME, 64 * 1024);
        assert_eq!(frame_size(MAX_FRAME).unwrap(), MAX_FRAME);
        assert!(frame_size(MAX_FRAME + 1).is_err());
    }

    #[test]
    fn hello_has_one_exact_encoding() {
        let controller = SecretKey::from_bytes(&[1; 32]);
        let agent = SecretKey::from_bytes(&[2; 32]);
        let hello = server_hello(&agent, &controller.public(), [3; 32]);
        assert!(parse_server_hello(&hello[..127]).is_err());
        assert!(parse_server_hello(&hello).is_ok());
        let mut trailing = hello;
        trailing.push(0);
        assert!(parse_server_hello(&trailing).is_err());
    }

    #[test]
    fn credentials_are_authenticated_and_confined_to_network_syntax() {
        let key = session_key([7; 32], b"transcript");
        let clear = network_conf("lab", "correct horse").unwrap().into_bytes();
        let cipher = crypt(&key, 0, b"transcript", &clear).unwrap();
        assert_eq!(
            String::from_utf8(decrypt(&key, 0, b"transcript", &cipher).unwrap()).unwrap(),
            "network={\n\tssid=6c6162\n\tpsk=\"correct horse\"\n\tkey_mgmt=WPA-PSK\n}\n"
        );
        assert!(decrypt(&key, 0, b"other transcript", &cipher).is_err());
        let mut corrupt = cipher;
        corrupt[0] ^= 1;
        assert!(decrypt(&key, 0, b"transcript", &corrupt).is_err());
        assert!(network_conf("x\nnetwork={", "12345678").is_err());
        assert!(network_conf("", "12345678").is_err());
        assert!(network_conf(&"x".repeat(33), "12345678").is_err());
        assert!(network_conf(&"x".repeat(32), "12345678").is_ok());
        assert!(network_conf("lab\r", "12345678").is_err());
        assert!(network_conf("lab\0", "12345678").is_err());
        assert!(network_conf("lab\"", "12345678").is_err());
        assert!(network_conf("lab\\", "12345678").is_err());
        assert!(network_conf("lab", "1234567\n").is_err());
        assert!(network_conf("lab", "1234567\r").is_err());
        assert!(network_conf("lab", "1234567").is_err());
        assert!(network_conf("lab", "12345678").is_ok());
        assert!(network_conf("lab", "1234567\0").is_err());
        assert!(network_conf("lab", &"g".repeat(64)).is_err());
        assert!(network_conf("lab", &"a".repeat(63))
            .unwrap()
            .contains("psk=\""));
        assert!(network_conf("lab", &"ab".repeat(32))
            .unwrap()
            .contains("psk=abab"));
        assert!(network_conf("lab\\\"", "tab\tpass").is_err());
        assert!(network_conf("lab", "quote\"pass").is_err());
        assert!(network_conf("lab", "slash\\pass").is_err());
        assert!(network_conf("a\tb", "12345678")
            .unwrap()
            .contains("ssid=610962\n"));
    }

    #[test]
    fn install_is_private_atomic_and_cleans_up_failures() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("network.conf");
        install(&output, b"new").unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&output).unwrap().permissions().mode() & 0o777, 0o600);

        let target = root.path().join("target");
        std::fs::write(&target, b"unchanged").unwrap();
        std::fs::remove_file(&output).unwrap();
        symlink(&target, &output).unwrap();
        install(&output, b"replacement").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        assert_eq!(std::fs::read(&output).unwrap(), b"replacement");
        assert!(!std::fs::symlink_metadata(&output)
            .unwrap()
            .file_type()
            .is_symlink());

        let blocked = root.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        assert!(install(&blocked, b"no").is_err());
        let mut staging = blocked.as_os_str().to_os_string();
        staging.push(format!(".ble.tmp.{}", std::process::id()));
        assert!(!std::path::Path::new(&staging).exists());

        std::fs::write(&staging, b"sentinel").unwrap();
        assert!(install(&blocked, b"no").is_err());
        assert_eq!(std::fs::read(&staging).unwrap(), b"sentinel");
    }

    #[test]
    fn advertisement_name_is_derived_from_the_agent_node_id() {
        let id = SecretKey::from_bytes(&[9; 32]).public();
        let advertisement = advertisement(&id);
        assert_eq!(
            advertisement.local_name,
            Some(format!("egdod-{}", &id.to_string()[..8]))
        );
        assert!(advertisement.service_uuids.contains(&SERVICE_UUID));
        assert_eq!(advertisement.discoverable, Some(true));
    }
}
