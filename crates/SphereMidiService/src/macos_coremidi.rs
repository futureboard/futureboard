//! Thin CoreMIDI bindings for macOS MIDI port scan and hardware I/O.
//!
//! Avoids the Rust `midir`/`coremidi` crates so we do not fight gpui's pinned
//! `core-foundation` version. Links Apple's CoreMIDI + CoreFoundation frameworks
//! directly.

use crate::{
    DetectedMidiDevice, HardwareMidiInputMessage, MidiDeviceDirection,
    coalesce_detected_midi_devices, decode_midi_bytes, stable_id, stable_id_ordinal,
};
use std::ffi::c_void;
use std::os::raw::{c_char, c_int, c_ulong};
use std::ptr;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;

type OSStatus = c_int;
type MIDIObjectRef = u32;
type MIDIClientRef = u32;
type MIDIPortRef = u32;
type MIDIEndpointRef = u32;
type MIDITimeStamp = u64;
type CFStringRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFIndex = isize;
type CFStringEncoding = u32;

const K_MIDI_PROPERTY_NAME: &[u8] = b"name\0";
const K_CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;
const MIDI_PACKET_LIST_BUF: usize = 512;

#[repr(C)]
struct MIDIPacket {
    time_stamp: MIDITimeStamp,
    length: u16,
    data: [u8; 256],
}

#[repr(C)]
struct MIDIPacketList {
    num_packets: u32,
    packet: MIDIPacket,
}

#[link(name = "CoreMIDI", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn MIDIGetNumberOfSources() -> c_ulong;
    fn MIDIGetSource(source_index_0: c_ulong) -> MIDIEndpointRef;
    fn MIDIGetNumberOfDestinations() -> c_ulong;
    fn MIDIGetDestination(dest_index_0: c_ulong) -> MIDIEndpointRef;
    fn MIDIObjectGetStringProperty(
        obj: MIDIObjectRef,
        property_id: CFStringRef,
        str: *mut CFStringRef,
    ) -> OSStatus;
    fn MIDIClientCreate(
        name: CFStringRef,
        notify_proc: Option<unsafe extern "C" fn(*const MIDINotification, *mut c_void)>,
        notify_ref_con: *mut c_void,
        out_client: *mut MIDIClientRef,
    ) -> OSStatus;
    fn MIDIClientDispose(client: MIDIClientRef) -> OSStatus;
    fn MIDIInputPortCreate(
        client: MIDIClientRef,
        port_name: CFStringRef,
        read_proc: Option<unsafe extern "C" fn(*const MIDIPacketList, *mut c_void, *mut c_void)>,
        ref_con: *mut c_void,
        out_port: *mut MIDIPortRef,
    ) -> OSStatus;
    fn MIDIOutputPortCreate(
        client: MIDIClientRef,
        port_name: CFStringRef,
        out_port: *mut MIDIPortRef,
    ) -> OSStatus;
    fn MIDIPortConnectSource(
        port: MIDIPortRef,
        source: MIDIEndpointRef,
        conn_ref_con: *mut c_void,
    ) -> OSStatus;
    fn MIDIPortDisconnectSource(port: MIDIPortRef, source: MIDIEndpointRef) -> OSStatus;
    fn MIDISend(
        port: MIDIPortRef,
        dest: MIDIEndpointRef,
        pktlist: *const MIDIPacketList,
    ) -> OSStatus;
    fn MIDIPacketListInit(pktlist: *mut MIDIPacketList) -> *mut MIDIPacket;
    fn MIDIPacketListAdd(
        pktlist: *mut MIDIPacketList,
        list_size: c_ulong,
        cur_packet: *mut MIDIPacket,
        time: MIDITimeStamp,
        n_data: c_ulong,
        data: *const u8,
    ) -> *mut MIDIPacket;

    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: CFStringEncoding,
    ) -> CFStringRef;
    fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: CFIndex,
        encoding: CFStringEncoding,
    ) -> u8;
    fn CFRelease(cf: *const c_void);
}

/// CoreMIDI notification message ids we care about. Any of these means the port
/// inventory moved, so a cached device list is stale.
const K_MIDI_MSG_SETUP_CHANGED: i32 = 1;
const K_MIDI_MSG_OBJECT_ADDED: i32 = 2;
const K_MIDI_MSG_OBJECT_REMOVED: i32 = 3;

#[repr(C)]
struct MIDINotification {
    message_id: i32,
    message_size: u32,
}

/// The process-wide CoreMIDI client, and whether the inventory changed since it
/// was last checked.
static CLIENT: AtomicU32 = AtomicU32::new(0);
static CLIENT_INIT: Once = Once::new();
static PORTS_CHANGED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn midi_notify_proc(message: *const MIDINotification, _ref_con: *mut c_void) {
    if message.is_null() {
        return;
    }
    // SAFETY: CoreMIDI passes a live notification for the duration of the call.
    let message_id = unsafe { (*message).message_id };
    if matches!(
        message_id,
        K_MIDI_MSG_SETUP_CHANGED | K_MIDI_MSG_OBJECT_ADDED | K_MIDI_MSG_OBJECT_REMOVED
    ) {
        PORTS_CHANGED.store(true, Ordering::Release);
    }
}

/// The shared CoreMIDI client, created once per process.
///
/// **This is what makes enumeration work at all.** CoreMIDI connects a process
/// to `MIDIServer` lazily, when it first creates a client; until then
/// `MIDIGetNumberOfSources` / `MIDIGetNumberOfDestinations` report **zero** even
/// with hardware attached. Every other CoreMIDI implementation (RtMidi, midir's
/// CoreMIDI backend) creates a client before enumerating for this reason.
///
/// The client is deliberately never disposed: it is the process's connection to
/// the MIDI server, and tearing it down would take the port inventory with it.
/// Returns 0 when creation failed, which callers treat as "enumerate anyway" —
/// a failed bootstrap must degrade to the previous behaviour, not to a panic.
fn shared_client() -> MIDIClientRef {
    CLIENT_INIT.call_once(|| {
        let name = cfstr("Futureboard");
        if name.is_null() {
            eprintln!("[MIDI scan] CoreMIDI client name allocation failed");
            return;
        }
        let mut client: MIDIClientRef = 0;
        let status =
            unsafe { MIDIClientCreate(name, Some(midi_notify_proc), ptr::null_mut(), &mut client) };
        unsafe { CFRelease(name) };
        if status != 0 || client == 0 {
            // -10844 is kMIDIServerStartErr; anything non-zero means the
            // process could not reach MIDIServer.
            eprintln!(
                "[MIDI scan] MIDIClientCreate failed (status {status}); CoreMIDI enumeration \
                 will report no devices"
            );
            return;
        }
        CLIENT.store(client, Ordering::Release);
        if crate::midi_settings_debug_enabled() {
            eprintln!("[MIDI scan] CoreMIDI client created (ref {client})");
        }
    });
    CLIENT.load(Ordering::Acquire)
}

/// True when CoreMIDI reported an added/removed port since the last call, and
/// clears the flag. Notifications are delivered on the run loop of the thread
/// that created the client, so this reports changes only when that thread runs
/// a run loop — it is a hint for refreshing a cache, never the sole path to a
/// correct device list.
pub fn take_ports_changed() -> bool {
    PORTS_CHANGED.swap(false, Ordering::AcqRel)
}

fn cfstr(s: &str) -> CFStringRef {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    unsafe {
        CFStringCreateWithCString(
            ptr::null(),
            bytes.as_ptr() as *const c_char,
            K_CF_STRING_ENCODING_UTF8,
        )
    }
}

fn cfstring_to_rust(cf: CFStringRef) -> Option<String> {
    if cf.is_null() {
        return None;
    }
    let mut buf = [0i8; 512];
    let ok = unsafe {
        CFStringGetCString(
            cf,
            buf.as_mut_ptr(),
            buf.len() as CFIndex,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
    Some(cstr.to_string_lossy().into_owned())
}

fn endpoint_name(endpoint: MIDIEndpointRef) -> Option<String> {
    if endpoint == 0 {
        return None;
    }
    let prop = unsafe {
        CFStringCreateWithCString(
            ptr::null(),
            K_MIDI_PROPERTY_NAME.as_ptr() as *const c_char,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    if prop.is_null() {
        return None;
    }
    let mut name_ref: CFStringRef = ptr::null();
    let status = unsafe { MIDIObjectGetStringProperty(endpoint, prop, &mut name_ref) };
    unsafe { CFRelease(prop) };
    if status != 0 || name_ref.is_null() {
        return None;
    }
    let name = cfstring_to_rust(name_ref);
    unsafe { CFRelease(name_ref) };
    name
}

pub fn scan_ports() -> Vec<DetectedMidiDevice> {
    // Establish the MIDIServer connection first. Without it CoreMIDI reports
    // zero endpoints regardless of what is plugged in.
    let client = shared_client();
    let mut devices = Vec::new();
    unsafe {
        let n_src = MIDIGetNumberOfSources();
        for i in 0..n_src {
            let src = MIDIGetSource(i);
            if let Some(name) = endpoint_name(src).map(|name| crate::normalize_port_name(&name)) {
                devices.push(DetectedMidiDevice {
                    id: stable_id(MidiDeviceDirection::Input, &name),
                    name,
                    direction: MidiDeviceDirection::Input,
                });
            }
        }
        let n_dst = MIDIGetNumberOfDestinations();
        for i in 0..n_dst {
            let dst = MIDIGetDestination(i);
            if let Some(name) = endpoint_name(dst).map(|name| crate::normalize_port_name(&name)) {
                devices.push(DetectedMidiDevice {
                    id: stable_id(MidiDeviceDirection::Output, &name),
                    name,
                    direction: MidiDeviceDirection::Output,
                });
            }
        }
        if crate::midi_settings_debug_enabled() {
            eprintln!(
                "[MIDI scan] CoreMIDI client={client} sources={n_src} destinations={} named={}",
                MIDIGetNumberOfDestinations(),
                devices.len()
            );
        }
        // Endpoints exist but none of them yielded a name: the list would be
        // silently empty, so say why rather than looking like "no hardware".
        if devices.is_empty() && (n_src > 0 || MIDIGetNumberOfDestinations() > 0) {
            eprintln!(
                "[MIDI scan] CoreMIDI reported endpoints but no readable names; \
                 no MIDI devices will be listed"
            );
        }
    }
    coalesce_detected_midi_devices(devices)
}

struct InputCallbackState {
    tx: Sender<HardwareMidiInputMessage>,
    device_id: String,
    device_name: String,
}

unsafe extern "C" fn midi_read_proc(
    pktlist: *const MIDIPacketList,
    read_proc_ref_con: *mut c_void,
    _src_conn_ref_con: *mut c_void,
) {
    if pktlist.is_null() || read_proc_ref_con.is_null() {
        return;
    }
    // SAFETY: CoreMIDI invokes this with a live packet list and the ref_con we
    // registered (owned by MacMidiInputConnection for the port lifetime).
    unsafe {
        let state = &*(read_proc_ref_con as *const InputCallbackState);
        let list = &*pktlist;
        let mut packet = &list.packet as *const MIDIPacket;
        for _ in 0..list.num_packets {
            if packet.is_null() {
                break;
            }
            let p = &*packet;
            let len = p.length as usize;
            let data = &p.data[..len.min(p.data.len())];
            if let Some(event) = decode_midi_bytes(data) {
                let _ = state.tx.send(HardwareMidiInputMessage {
                    device_id: state.device_id.clone(),
                    device_name: state.device_name.clone(),
                    event,
                    received_at: std::time::Instant::now(),
                });
            }
            // Advance to next packet (variable-length: header + data[length], 4-byte aligned).
            let packet_bytes =
                std::mem::size_of::<MIDITimeStamp>() + std::mem::size_of::<u16>() + len;
            let aligned = (packet_bytes + 3) & !3;
            packet = (packet as *const u8).add(aligned) as *const MIDIPacket;
        }
    }
}

/// Keeps a CoreMIDI input port connected for the lifetime of the connection.
pub struct MacMidiInputConnection {
    client: MIDIClientRef,
    port: MIDIPortRef,
    source: MIDIEndpointRef,
    _callback: Box<InputCallbackState>,
}

impl Drop for MacMidiInputConnection {
    fn drop(&mut self) {
        unsafe {
            if self.port != 0 && self.source != 0 {
                let _ = MIDIPortDisconnectSource(self.port, self.source);
            }
            if self.client != 0 {
                let _ = MIDIClientDispose(self.client);
            }
        }
    }
}

fn find_source_by_name_or_id(device_id: &str, device_name: &str) -> Option<MIDIEndpointRef> {
    // Same bootstrap as the scan: without a client there are no endpoints to
    // find, so opening would fail with a misleading "source not found".
    let _ = shared_client();
    let ordinal = stable_id_ordinal(device_id, device_name);
    let mut occurrence = 0usize;
    unsafe {
        let n = MIDIGetNumberOfSources();
        for i in 0..n {
            let src = MIDIGetSource(i);
            if endpoint_name(src)
                .map(|name| crate::normalize_port_name(&name))
                .as_deref()
                == Some(device_name)
            {
                occurrence += 1;
                if occurrence == ordinal {
                    return Some(src);
                }
            }
        }
    }
    None
}

fn find_destination_by_name_or_id(device_id_or_name: &str) -> Option<(MIDIEndpointRef, String)> {
    let _ = shared_client();
    let mut occurrences = std::collections::HashMap::<String, usize>::new();
    unsafe {
        let n = MIDIGetNumberOfDestinations();
        for i in 0..n {
            let dst = MIDIGetDestination(i);
            if let Some(name) = endpoint_name(dst).map(|name| crate::normalize_port_name(&name)) {
                let stable = stable_id(MidiDeviceDirection::Output, &name);
                let stable_io = stable_id(MidiDeviceDirection::InputOutput, &name);
                let occurrence = occurrences.entry(name.clone()).or_insert(0);
                *occurrence += 1;
                let ordinal = stable_id_ordinal(device_id_or_name, &name);
                if name == device_id_or_name
                    || ((stable == device_id_or_name
                        || stable_io == device_id_or_name
                        || ordinal > 1)
                        && *occurrence == ordinal)
                {
                    return Some((dst, name));
                }
            }
        }
    }
    None
}

pub fn open_inputs(
    enabled: Vec<(String, String)>,
    tx: Sender<HardwareMidiInputMessage>,
) -> Vec<(String, MacMidiInputConnection)> {
    let mut connections = Vec::new();
    for (device_id, device_name) in enabled {
        let Some(source) = find_source_by_name_or_id(&device_id, &device_name) else {
            eprintln!("[MIDI input] CoreMIDI source not found for '{device_name}'");
            continue;
        };
        let mut callback = Box::new(InputCallbackState {
            tx: tx.clone(),
            device_id: device_id.clone(),
            device_name: device_name.clone(),
        });
        let callback_ptr = (&mut *callback) as *mut InputCallbackState as *mut c_void;

        let mut client: MIDIClientRef = 0;
        let client_name = cfstr(&format!("Futureboard MIDI in ({device_name})"));
        if client_name.is_null() {
            continue;
        }
        let status = unsafe { MIDIClientCreate(client_name, None, ptr::null_mut(), &mut client) };
        unsafe { CFRelease(client_name) };
        if status != 0 || client == 0 {
            eprintln!("[MIDI input] MIDIClientCreate failed for '{device_name}': {status}");
            continue;
        }

        let mut port: MIDIPortRef = 0;
        let port_name = cfstr(&format!("Futureboard listen ({device_name})"));
        if port_name.is_null() {
            unsafe {
                let _ = MIDIClientDispose(client);
            }
            continue;
        }
        let status = unsafe {
            MIDIInputPortCreate(
                client,
                port_name,
                Some(midi_read_proc),
                callback_ptr,
                &mut port,
            )
        };
        unsafe { CFRelease(port_name) };
        if status != 0 || port == 0 {
            eprintln!("[MIDI input] MIDIInputPortCreate failed for '{device_name}': {status}");
            unsafe {
                let _ = MIDIClientDispose(client);
            }
            continue;
        }

        let status = unsafe { MIDIPortConnectSource(port, source, ptr::null_mut()) };
        if status != 0 {
            eprintln!("[MIDI input] MIDIPortConnectSource failed for '{device_name}': {status}");
            unsafe {
                let _ = MIDIClientDispose(client);
            }
            continue;
        }

        if crate::midi_settings_debug_enabled() {
            eprintln!("[MIDI input] CoreMIDI connected '{device_name}' ({device_id})");
        }
        connections.push((
            device_id,
            MacMidiInputConnection {
                client,
                port,
                source,
                _callback: callback,
            },
        ));
    }
    connections
}

/// CoreMIDI output connection (client + port + destination).
pub struct MacMidiOutputConnection {
    client: MIDIClientRef,
    port: MIDIPortRef,
    dest: MIDIEndpointRef,
}

impl Drop for MacMidiOutputConnection {
    fn drop(&mut self) {
        unsafe {
            if self.client != 0 {
                let _ = MIDIClientDispose(self.client);
            }
        }
    }
}

impl MacMidiOutputConnection {
    pub fn send(&self, message: &[u8]) -> Result<(), OSStatus> {
        if message.is_empty() || message.len() > 256 {
            return Err(-1);
        }
        let mut buf = [0u8; MIDI_PACKET_LIST_BUF];
        let pktlist = buf.as_mut_ptr() as *mut MIDIPacketList;
        unsafe {
            let mut cur = MIDIPacketListInit(pktlist);
            cur = MIDIPacketListAdd(
                pktlist,
                MIDI_PACKET_LIST_BUF as c_ulong,
                cur,
                0,
                message.len() as c_ulong,
                message.as_ptr(),
            );
            if cur.is_null() {
                return Err(-1);
            }
            let status = MIDISend(self.port, self.dest, pktlist);
            if status != 0 { Err(status) } else { Ok(()) }
        }
    }
}

pub fn open_output(device_id_or_name: &str) -> Option<MacMidiOutputConnection> {
    let (dest, name) = find_destination_by_name_or_id(device_id_or_name)?;
    let mut client: MIDIClientRef = 0;
    let client_name = cfstr("Futureboard MIDI playback");
    if client_name.is_null() {
        return None;
    }
    let status = unsafe { MIDIClientCreate(client_name, None, ptr::null_mut(), &mut client) };
    unsafe { CFRelease(client_name) };
    if status != 0 || client == 0 {
        eprintln!("[MIDI output] MIDIClientCreate failed for '{name}': {status}");
        return None;
    }
    let mut port: MIDIPortRef = 0;
    let port_name = cfstr("Futureboard MIDI Out");
    if port_name.is_null() {
        unsafe {
            let _ = MIDIClientDispose(client);
        }
        return None;
    }
    let status = unsafe { MIDIOutputPortCreate(client, port_name, &mut port) };
    unsafe { CFRelease(port_name) };
    if status != 0 || port == 0 {
        eprintln!("[MIDI output] MIDIOutputPortCreate failed for '{name}': {status}");
        unsafe {
            let _ = MIDIClientDispose(client);
        }
        return None;
    }
    Some(MacMidiOutputConnection { client, port, dest })
}
