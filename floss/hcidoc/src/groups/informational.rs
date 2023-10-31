///! Rule group for general information.
use chrono::NaiveDateTime;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::convert::Into;
use std::fmt;
use std::hash::Hash;
use std::io::Write;

use crate::engine::{Rule, RuleGroup, Signal};
use crate::parser::{Packet, PacketChild};
use bt_packets::hci::{
    AclCommandChild, Address, CommandChild, ConnectionManagementCommandChild, DisconnectReason,
    ErrorCode, EventChild, GapData, GapDataType, LeMetaEventChild,
};

/// Valid values are in the range 0x0000-0x0EFF.
type ConnectionHandle = u16;

const INVALID_TS: NaiveDateTime = NaiveDateTime::MAX;

fn print_start_end_timestamps(start: NaiveDateTime, end: NaiveDateTime) -> String {
    fn print_time(ts: NaiveDateTime) -> String {
        if ts == INVALID_TS {
            return "N/A".to_owned();
        }
        return format!("{}", ts.time());
    }

    if start == end && start != INVALID_TS {
        return format!("{} - Failed", start.time());
    }
    return format!("{} to {}", print_time(start), print_time(end));
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord)]
enum AddressType {
    None,
    BREDR,
    LE,
    Dual,
}

impl fmt::Display for AddressType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let str = match self {
            AddressType::None => "Unknown type",
            AddressType::BREDR => "BR/EDR",
            AddressType::LE => "LE",
            AddressType::Dual => "Dual",
        };
        write!(f, "{}", str)
    }
}

impl AddressType {
    fn update(&mut self, new_type: AddressType) {
        *self = match self {
            AddressType::None => new_type,
            AddressType::Dual => AddressType::Dual,
            AddressType::BREDR => match new_type {
                AddressType::Dual | AddressType::LE => AddressType::Dual,
                _ => AddressType::BREDR,
            },
            AddressType::LE => match new_type {
                AddressType::Dual | AddressType::BREDR => AddressType::Dual,
                _ => AddressType::LE,
            },
        }
    }
}

enum InitiatorType {
    Unknown,
    Host,
    Peer,
}

impl fmt::Display for InitiatorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let str = match self {
            InitiatorType::Unknown => "Unknown initiator",
            InitiatorType::Host => "Host initiated",
            InitiatorType::Peer => "Peer initiated",
        };
        write!(f, "{}", str)
    }
}

#[derive(Copy, Clone)]
enum AclState {
    None,
    Initiating,
    Accepting,
    Connected,
}

impl Into<InitiatorType> for AclState {
    fn into(self) -> InitiatorType {
        match self {
            AclState::Initiating => InitiatorType::Host,
            AclState::Accepting => InitiatorType::Peer,
            _ => InitiatorType::Unknown,
        }
    }
}

/// Information about a specific device address
struct DeviceInformation {
    names: HashSet<String>,
    address: Address,
    address_type: AddressType,
    acls: Vec<AclInformation>,
    acl_state: AclState,
}

impl DeviceInformation {
    pub fn new(address: Address) -> Self {
        DeviceInformation {
            names: HashSet::new(),
            address: address,
            address_type: AddressType::None,
            acls: vec![],
            acl_state: AclState::None,
        }
    }

    fn is_connection_active(&self) -> bool {
        // not empty and last connection's end time is not set.
        return !self.acls.is_empty() && self.acls.last().unwrap().end_time == INVALID_TS;
    }

    fn get_or_allocate_connection(&mut self, handle: &ConnectionHandle) -> &mut AclInformation {
        if !self.is_connection_active() {
            let acl = AclInformation::new(*handle);
            self.acls.push(acl);
        }
        return self.acls.last_mut().unwrap();
    }

    fn report_connection_start(&mut self, handle: ConnectionHandle, ts: NaiveDateTime) {
        let mut acl = AclInformation::new(handle);
        let initiator = self.acl_state.into();
        acl.report_start(initiator, ts);
        self.acls.push(acl);
        self.acl_state = AclState::Connected;
    }

    fn report_connection_end(&mut self, handle: ConnectionHandle, ts: NaiveDateTime) {
        let acl = self.get_or_allocate_connection(&handle);
        acl.report_end(ts);
        self.acl_state = AclState::None;
    }

    fn print_names(names: &HashSet<String>) -> String {
        if names.len() > 1 {
            format!("{:?}", names)
        } else {
            names.iter().next().unwrap_or(&String::from("<Unknown name>")).to_owned()
        }
    }
}

impl fmt::Display for DeviceInformation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = writeln!(
            f,
            "{address} ({address_type}, {device_names}), {num_connections} connections",
            address = self.address,
            address_type = self.address_type,
            device_names = DeviceInformation::print_names(&self.names),
            num_connections = self.acls.len()
        );
        for acl in &self.acls {
            let _ = write!(f, "{}", acl);
        }

        Ok(())
    }
}

/// Information for an ACL connection session
struct AclInformation {
    start_time: NaiveDateTime,
    end_time: NaiveDateTime,
    handle: ConnectionHandle,
    initiator: InitiatorType,
    active_profiles: HashMap<ProfileId, ProfileInformation>,
    inactive_profiles: Vec<ProfileInformation>,
}

impl AclInformation {
    pub fn new(handle: ConnectionHandle) -> Self {
        AclInformation {
            start_time: INVALID_TS,
            end_time: INVALID_TS,
            handle: handle,
            initiator: InitiatorType::Unknown,
            active_profiles: HashMap::new(),
            inactive_profiles: vec![],
        }
    }

    fn report_start(&mut self, initiator: InitiatorType, ts: NaiveDateTime) {
        self.initiator = initiator;
        self.start_time = ts;
    }

    fn report_end(&mut self, ts: NaiveDateTime) {
        // disconnect the active profiles
        for (_, mut profile) in self.active_profiles.drain() {
            profile.report_end(ts);
            self.inactive_profiles.push(profile);
        }
        self.end_time = ts;
    }

    fn report_profile_start(
        &mut self,
        profile_type: ProfileType,
        profile_id: ProfileId,
        initiator: InitiatorType,
        ts: NaiveDateTime,
    ) {
        let mut profile = ProfileInformation::new(profile_type);
        profile.report_start(initiator, ts);
        let old_profile = self.active_profiles.insert(profile_id, profile);
        if let Some(profile) = old_profile {
            self.inactive_profiles.push(profile);
        }
    }

    fn report_profile_end(
        &mut self,
        profile_type: ProfileType,
        profile_id: ProfileId,
        ts: NaiveDateTime,
    ) {
        let mut profile = self
            .active_profiles
            .remove(&profile_id)
            .unwrap_or(ProfileInformation::new(profile_type));
        profile.report_end(ts);
        self.inactive_profiles.push(profile);
    }
}

impl fmt::Display for AclInformation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = writeln!(
            f,
            "  Handle: {handle}, {initiator}, {timestamp_info}",
            handle = self.handle,
            initiator = self.initiator,
            timestamp_info = print_start_end_timestamps(self.start_time, self.end_time)
        );

        for profile in self.inactive_profiles.iter() {
            let _ = write!(f, "{}", profile);
        }
        for (_, profile) in self.active_profiles.iter() {
            let _ = write!(f, "{}", profile);
        }

        Ok(())
    }
}

// Currently only HFP is possible to be detected. Other profiles needs us to parse L2CAP packets.
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
enum ProfileType {
    HFP,
}

impl fmt::Display for ProfileType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let str = match self {
            ProfileType::HFP => "HFP",
        };
        write!(f, "{}", str)
    }
}

// Use to distinguish between the same profiles within one ACL connection.
// Currently only for profiles that's guaranteed to be singular, i.e. HFP.
// Later we can add L2CAP's CID and RFCOMM's DLCI, for example.
#[derive(Eq, Hash, PartialEq)]
enum ProfileId {
    OnePerConnection(ProfileType),
}

struct ProfileInformation {
    start_time: NaiveDateTime,
    end_time: NaiveDateTime,
    profile_type: ProfileType,
    initiator: InitiatorType,
}

impl ProfileInformation {
    pub fn new(profile_type: ProfileType) -> Self {
        ProfileInformation {
            start_time: INVALID_TS,
            end_time: INVALID_TS,
            profile_type: profile_type,
            initiator: InitiatorType::Unknown,
        }
    }

    fn report_start(&mut self, initiator: InitiatorType, ts: NaiveDateTime) {
        self.initiator = initiator;
        self.start_time = ts;
    }

    fn report_end(&mut self, ts: NaiveDateTime) {
        self.end_time = ts;
    }
}

impl fmt::Display for ProfileInformation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "    {profile}, {initiator}, {timestamp_info}",
            profile = self.profile_type,
            initiator = self.initiator,
            timestamp_info = print_start_end_timestamps(self.start_time, self.end_time)
        )
    }
}

/// This rule prints devices names and connection/disconnection time.
struct InformationalRule {
    devices: HashMap<Address, DeviceInformation>,
    handles: HashMap<ConnectionHandle, Address>,
    sco_handles: HashMap<ConnectionHandle, ConnectionHandle>,
    /// unknownConnections store connections which is initiated before btsnoop starts.
    unknown_connections: HashMap<ConnectionHandle, AclInformation>,
    /// When powering off, the controller might or might not reply disconnection request. Therefore
    /// make this a special case.
    pending_disconnect_due_to_host_power_off: HashSet<ConnectionHandle>,
}

impl InformationalRule {
    pub fn new() -> Self {
        InformationalRule {
            devices: HashMap::new(),
            handles: HashMap::new(),
            sco_handles: HashMap::new(),
            unknown_connections: HashMap::new(),
            pending_disconnect_due_to_host_power_off: HashSet::new(),
        }
    }

    fn get_or_allocate_device(&mut self, address: &Address) -> &mut DeviceInformation {
        if !self.devices.contains_key(address) {
            self.devices.insert(*address, DeviceInformation::new(*address));
        }
        return self.devices.get_mut(address).unwrap();
    }

    fn get_or_allocate_unknown_connection(
        &mut self,
        handle: &ConnectionHandle,
    ) -> &mut AclInformation {
        if !self.unknown_connections.contains_key(handle) {
            self.unknown_connections.insert(*handle, AclInformation::new(*handle));
        }
        return self.unknown_connections.get_mut(handle).unwrap();
    }

    fn get_or_allocate_connection(&mut self, handle: &ConnectionHandle) -> &mut AclInformation {
        if !self.handles.contains_key(&handle) {
            let conn = self.get_or_allocate_unknown_connection(&handle);
            return conn;
        }

        let address = &self.handles.get(handle).unwrap().clone();
        let device = self.get_or_allocate_device(address);
        return device.get_or_allocate_connection(handle);
    }

    fn report_address_type(&mut self, address: &Address, address_type: AddressType) {
        let device = self.get_or_allocate_device(address);
        device.address_type.update(address_type);
    }

    fn report_name(&mut self, address: &Address, name: &String) {
        let device = self.get_or_allocate_device(address);
        device.names.insert(name.into());
    }

    fn report_acl_state(&mut self, address: &Address, state: AclState) {
        let device = self.get_or_allocate_device(address);
        device.acl_state = state;
    }

    fn report_connection_start(
        &mut self,
        address: &Address,
        handle: ConnectionHandle,
        ts: NaiveDateTime,
    ) {
        let device = self.get_or_allocate_device(address);
        device.report_connection_start(handle, ts);
        self.handles.insert(handle, *address);
    }

    fn report_sco_connection_start(
        &mut self,
        address: &Address,
        handle: ConnectionHandle,
        ts: NaiveDateTime,
    ) {
        if !self.devices.contains_key(address) {
            // To simplify things, let's not process unknown devices
            return;
        }

        let device = self.devices.get_mut(address).unwrap();
        if !device.is_connection_active() {
            // SCO is connected, but ACL is not. This is weird, but let's ignore for simplicity.
            eprintln!("[{}] SCO is connected, but ACL is not.", address);
            return;
        }

        // Whatever handle value works here - we aren't allocating a new one.
        let acl = device.get_or_allocate_connection(&0);
        let acl_handle = acl.handle;
        // We need to listen the HCI commands to determine the correct initiator.
        // Here we just assume host for simplicity.
        acl.report_profile_start(
            ProfileType::HFP,
            ProfileId::OnePerConnection(ProfileType::HFP),
            InitiatorType::Host,
            ts,
        );

        self.sco_handles.insert(handle, acl_handle);
    }

    fn report_connection_end(&mut self, handle: ConnectionHandle, ts: NaiveDateTime) {
        // This might be a SCO disconnection event, so check that first
        if self.sco_handles.contains_key(&handle) {
            let acl_handle = self.sco_handles[&handle];
            let conn = self.get_or_allocate_connection(&acl_handle);
            conn.report_profile_end(
                ProfileType::HFP,
                ProfileId::OnePerConnection(ProfileType::HFP),
                ts,
            );
            return;
        }

        // Not recognized as SCO, assume it's an ACL handle.
        if let Some(address) = self.handles.get(&handle) {
            // This device is known
            let device = self.devices.get_mut(address).unwrap();
            device.report_connection_end(handle, ts);
            self.handles.remove(&handle);

            // remove the associated SCO handle, if any
            self.sco_handles.retain(|_sco_handle, acl_handle| *acl_handle != handle);
        } else {
            // Unknown device.
            let conn = self.get_or_allocate_unknown_connection(&handle);
            conn.report_end(ts);
        }
    }

    fn report_reset(&mut self, ts: NaiveDateTime) {
        // report_connection_end removes the entries from the map, so store all the keys first.
        let handles: Vec<ConnectionHandle> = self.handles.keys().cloned().collect();
        for handle in handles {
            self.report_connection_end(handle, ts);
        }
        self.sco_handles.clear();
        self.pending_disconnect_due_to_host_power_off.clear();
    }

    fn _report_profile_start(
        &mut self,
        handle: ConnectionHandle,
        profile_type: ProfileType,
        profile_id: ProfileId,
        initiator: InitiatorType,
        ts: NaiveDateTime,
    ) {
        let conn = self.get_or_allocate_connection(&handle);
        conn.report_profile_start(profile_type, profile_id, initiator, ts);
    }

    fn _report_profile_end(
        &mut self,
        handle: ConnectionHandle,
        profile_type: ProfileType,
        profile_id: ProfileId,
        ts: NaiveDateTime,
    ) {
        let conn = self.get_or_allocate_connection(&handle);
        conn.report_profile_end(profile_type, profile_id, ts);
    }

    fn process_gap_data(&mut self, address: &Address, data: &GapData) {
        match data.data_type {
            GapDataType::CompleteLocalName | GapDataType::ShortenedLocalName => {
                let name = String::from_utf8_lossy(data.data.as_slice()).into_owned();
                self.report_name(address, &name);
            }

            _ => {}
        }
    }

    fn process_raw_gap_data(&mut self, address: &Address, data: &[u8]) {
        let mut offset = 0;
        while offset < data.len() {
            let chunk_size = usize::from(data[offset]);
            let chunk_end = offset + chunk_size + 1;

            // Prevent out-of-bounds index
            if chunk_end > data.len() {
                return;
            }
            match GapData::parse(&data[offset..chunk_end]) {
                Ok(gap_data) => self.process_gap_data(&address, &gap_data),
                Err(_err) => {}
            }
            offset = chunk_end;
        }
    }
}

impl Rule for InformationalRule {
    fn process(&mut self, packet: &Packet) {
        match &packet.inner {
            PacketChild::HciEvent(ev) => match ev.specialize() {
                EventChild::ConnectionComplete(ev) => {
                    self.report_connection_start(
                        &ev.get_bd_addr(),
                        ev.get_connection_handle(),
                        packet.ts,
                    );

                    // If failed, assume it's the end of connection.
                    if ev.get_status() != ErrorCode::Success {
                        self.report_connection_end(ev.get_connection_handle(), packet.ts);
                    }
                }

                EventChild::SynchronousConnectionComplete(ev) => {
                    self.report_sco_connection_start(
                        &ev.get_bd_addr(),
                        ev.get_connection_handle(),
                        packet.ts,
                    );
                    // If failed, assume it's the end of connection.
                    if ev.get_status() != ErrorCode::Success {
                        self.report_connection_end(ev.get_connection_handle(), packet.ts);
                    }
                }

                EventChild::DisconnectionComplete(ev) => {
                    // If disconnected because host is powering off, the event has been processed.
                    // We can't just query the reason here because it's different across vendors.
                    if !self
                        .pending_disconnect_due_to_host_power_off
                        .remove(&ev.get_connection_handle())
                    {
                        self.report_connection_end(ev.get_connection_handle(), packet.ts);
                    }
                }

                EventChild::ExtendedInquiryResult(ev) => {
                    for data in ev.get_extended_inquiry_response() {
                        self.process_gap_data(&ev.get_address(), data);
                    }
                    self.report_address_type(&ev.get_address(), AddressType::BREDR);
                }

                EventChild::RemoteNameRequestComplete(ev) => {
                    if ev.get_status() != ErrorCode::Success {
                        return;
                    }
                    let name = String::from_utf8_lossy(ev.get_remote_name());
                    let name = name.trim_end_matches(char::from(0));
                    self.report_name(&ev.get_bd_addr(), &name.to_owned());
                    self.report_address_type(&ev.get_bd_addr(), AddressType::BREDR);
                }

                EventChild::LeMetaEvent(ev) => match ev.specialize() {
                    LeMetaEventChild::LeConnectionComplete(ev) => {
                        if ev.get_status() != ErrorCode::Success {
                            return;
                        }

                        // Determining LE initiator is complex, for simplicity assume host inits.
                        self.report_acl_state(&ev.get_peer_address(), AclState::Initiating);
                        self.report_connection_start(
                            &ev.get_peer_address(),
                            ev.get_connection_handle(),
                            packet.ts,
                        );
                        self.report_address_type(&ev.get_peer_address(), AddressType::LE);
                    }

                    LeMetaEventChild::LeEnhancedConnectionComplete(ev) => {
                        if ev.get_status() != ErrorCode::Success {
                            return;
                        }

                        // Determining LE initiator is complex, for simplicity assume host inits.
                        self.report_acl_state(&ev.get_peer_address(), AclState::Initiating);
                        self.report_connection_start(
                            &ev.get_peer_address(),
                            ev.get_connection_handle(),
                            packet.ts,
                        );
                        self.report_address_type(&ev.get_peer_address(), AddressType::LE);
                    }

                    // Use the Raw version because somehow LeAdvertisingReport doesn't work
                    LeMetaEventChild::LeAdvertisingReportRaw(ev) => {
                        for resp in ev.get_responses() {
                            self.process_raw_gap_data(&resp.address, &resp.advertising_data);
                            self.report_address_type(&resp.address, AddressType::LE);
                        }
                    }

                    // Use the Raw version because somehow LeExtendedAdvertisingReport doesn't work
                    LeMetaEventChild::LeExtendedAdvertisingReportRaw(ev) => {
                        for resp in ev.get_responses() {
                            self.process_raw_gap_data(&resp.address, &resp.advertising_data);
                            self.report_address_type(&resp.address, AddressType::LE);
                        }
                    }

                    // EventChild::LeMetaEvent(ev).specialize()
                    _ => {}
                },

                // PacketChild::HciEvent(ev) => match ev.specialize()
                _ => {}
            },

            PacketChild::HciCommand(cmd) => match cmd.specialize() {
                CommandChild::Reset(_cmd) => {
                    self.report_reset(packet.ts);
                }

                CommandChild::AclCommand(cmd) => match cmd.specialize() {
                    AclCommandChild::ConnectionManagementCommand(cmd) => match cmd.specialize() {
                        ConnectionManagementCommandChild::CreateConnection(cmd) => {
                            self.report_acl_state(&cmd.get_bd_addr(), AclState::Initiating);
                            self.report_address_type(&cmd.get_bd_addr(), AddressType::BREDR);
                        }

                        ConnectionManagementCommandChild::AcceptConnectionRequest(cmd) => {
                            self.report_acl_state(&cmd.get_bd_addr(), AclState::Accepting);
                            self.report_address_type(&cmd.get_bd_addr(), AddressType::BREDR);
                        }

                        // AclCommandChild::ConnectionManagementCommand(cmd).specialize()
                        _ => {}
                    },

                    AclCommandChild::Disconnect(cmd) => {
                        // If reason is power off, the host might not wait for connection complete event
                        if cmd.get_reason()
                            == DisconnectReason::RemoteDeviceTerminatedConnectionPowerOff
                        {
                            self.pending_disconnect_due_to_host_power_off
                                .insert(cmd.get_connection_handle());
                            self.report_connection_end(cmd.get_connection_handle(), packet.ts);
                        }
                    }

                    // CommandChild::AclCommand(cmd).specialize()
                    _ => {}
                },

                // PacketChild::HciCommand(cmd).specialize()
                _ => {}
            },

            // packet.inner
            _ => {}
        }
    }

    fn report(&self, writer: &mut dyn Write) {
        /* Sort when displaying the addresses, from the most to the least important:
         * (1) Device with connections > Device without connections
         * (2) Device with known name > Device with unknown name
         * (3) BREDR > LE > Dual
         * (4) Name, lexicographically (case sensitive)
         * (5) Address, alphabetically
         */
        fn sort_addresses(a: &DeviceInformation, b: &DeviceInformation) -> Ordering {
            let connection_order = a.acls.is_empty().cmp(&b.acls.is_empty());
            if connection_order != Ordering::Equal {
                return connection_order;
            }

            let known_name_order = a.names.is_empty().cmp(&b.names.is_empty());
            if known_name_order != Ordering::Equal {
                return known_name_order;
            }

            let address_type_order = a.address_type.cmp(&b.address_type);
            if address_type_order != Ordering::Equal {
                return address_type_order;
            }

            let a_name = format!("{}", DeviceInformation::print_names(&a.names));
            let b_name = format!("{}", DeviceInformation::print_names(&b.names));
            let name_order = a_name.cmp(&b_name);
            if name_order != Ordering::Equal {
                return name_order;
            }

            let a_address = <[u8; 6]>::from(a.address);
            let b_address = <[u8; 6]>::from(b.address);
            for i in (0..6).rev() {
                let address_order = a_address[i].cmp(&b_address[i]);
                if address_order != Ordering::Equal {
                    return address_order;
                }
            }
            // This shouldn't be executed
            return Ordering::Equal;
        }

        if self.devices.is_empty() && self.unknown_connections.is_empty() {
            return;
        }

        let mut addresses: Vec<Address> = self.devices.keys().cloned().collect();
        addresses.sort_unstable_by(|a, b| sort_addresses(&self.devices[a], &self.devices[b]));

        let _ = writeln!(writer, "InformationalRule report:");
        if !self.unknown_connections.is_empty() {
            let _ = writeln!(
                writer,
                "Connections initiated before snoop start, {} connections",
                self.unknown_connections.len()
            );
            for (_, acl) in &self.unknown_connections {
                let _ = write!(writer, "{}", acl);
            }
        }
        for address in addresses {
            let _ = write!(writer, "{}", self.devices[&address]);
        }
    }

    fn report_signals(&self) -> &[Signal] {
        &[]
    }
}

/// Get a rule group with collision rules.
pub fn get_informational_group() -> RuleGroup {
    let mut group = RuleGroup::new();
    group.add_rule(Box::new(InformationalRule::new()));

    group
}
