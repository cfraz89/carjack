-- iAP2 (iPod Accessory Protocol 2) 
--
-- Layers:
--   1. iAP2 Link — packet framing, control flags, checksums
--   2. iAP2 Session — payload interpretation based on session type
--      a. Control Session messages with parameters
--      b. File Transfer Session datagrams
--      c. External Accessory Session datagrams

-- ============================================================
-- Protocol declarations
-- ============================================================

local iap2_proto = Proto("iap2", "iPod Accessory Protocol 2 (iAP2)")

-- ============================================================
-- Link Layer Fields
-- ============================================================

local f_link_sop        = ProtoField.uint16("iap2.link.sop",        "Start of Packet",              base.HEX)
local f_link_length     = ProtoField.uint16("iap2.link.length",     "Packet Length",                base.DEC)
local f_link_ctrl       = ProtoField.uint8 ("iap2.link.control",    "Control Byte",                 base.HEX)
local f_link_ctrl_syn   = ProtoField.bool  ("iap2.link.control.syn","SYN - Link Sync Payload",      8, nil, 0x80)
local f_link_ctrl_ack   = ProtoField.bool  ("iap2.link.control.ack","ACK - Acknowledgement Valid",  8, nil, 0x40)
local f_link_ctrl_eak   = ProtoField.bool  ("iap2.link.control.eak","EAK - Extended Ack Payload",   8, nil, 0x20)
local f_link_ctrl_rst   = ProtoField.bool  ("iap2.link.control.rst","RST - Link Reset",             8, nil, 0x10)
local f_link_ctrl_slp   = ProtoField.bool  ("iap2.link.control.slp","SLP - Device Sleep",           8, nil, 0x08)
local f_link_seq        = ProtoField.uint8 ("iap2.link.seq",        "Sequence Number",              base.DEC)
local f_link_ack        = ProtoField.uint8 ("iap2.link.ack",        "Acknowledgement Number",       base.DEC)
local f_link_session_id = ProtoField.uint8 ("iap2.link.session_id", "Session Identifier",           base.DEC)
local f_link_hdr_cksum  = ProtoField.uint8 ("iap2.link.hdr_cksum",  "Header Checksum",              base.HEX)
local f_link_hdr_cksum_status = ProtoField.string("iap2.link.hdr_cksum_status", "[Header Checksum Status]")
local f_link_pkt_type   = ProtoField.string("iap2.link.pkt_type",   "[Packet Type]")

-- Link Synchronization Payload
local f_syn_version     = ProtoField.uint8 ("iap2.syn.version",     "Link Version",                 base.DEC)
local f_syn_max_out     = ProtoField.uint8 ("iap2.syn.max_outstanding", "Max Outstanding Packets",  base.DEC)
local f_syn_max_len     = ProtoField.uint16("iap2.syn.max_length",  "Max Received Packet Length",   base.DEC)
local f_syn_retx_to     = ProtoField.uint16("iap2.syn.retx_timeout","Retransmission Timeout (ms)",  base.DEC)
local f_syn_cumack_to   = ProtoField.uint16("iap2.syn.cumack_timeout","Cumulative Ack Timeout (ms)",base.DEC)
local f_syn_max_retx    = ProtoField.uint8 ("iap2.syn.max_retx",    "Max Retransmissions",          base.DEC)
local f_syn_max_cumack  = ProtoField.uint8 ("iap2.syn.max_cumack",  "Max Cumulative Acks",          base.DEC)
local f_syn_link_mode   = ProtoField.string("iap2.syn.link_mode",   "[Link Mode]")
-- Session entries within SYN
local f_syn_sess_id     = ProtoField.uint8 ("iap2.syn.session.id",  "Session Identifier",           base.DEC)
local f_syn_sess_type   = ProtoField.uint8 ("iap2.syn.session.type","Session Type",                 base.DEC)
local f_syn_sess_ver    = ProtoField.uint8 ("iap2.syn.session.ver", "Session Version",              base.DEC)
local f_syn_sess_type_str = ProtoField.string("iap2.syn.session.type_str", "[Session Type Name]")

-- Extended Acknowledgement Payload
local f_eak_seq         = ProtoField.uint8 ("iap2.eak.seq",         "Out-of-Sequence PSN",          base.DEC)

-- Payload checksum
local f_payload_cksum   = ProtoField.uint8 ("iap2.payload_cksum",   "Payload Checksum",             base.HEX)
local f_payload_cksum_status = ProtoField.string("iap2.payload_cksum_status", "[Payload Checksum Status]")

-- ============================================================
-- Control Session Message Fields
-- ============================================================

local f_cs_msg_sop      = ProtoField.uint16("iap2.cs.sop",          "Start of Message",             base.HEX)
local f_cs_msg_length   = ProtoField.uint16("iap2.cs.length",       "Message Length",               base.DEC)
local f_cs_msg_id       = ProtoField.uint16("iap2.cs.id",           "Message ID",                   base.HEX)
local f_cs_msg_name     = ProtoField.string("iap2.cs.name",         "[Message Name]")
local f_cs_msg_source   = ProtoField.string("iap2.cs.source",       "[Source]")

-- Parameter fields
local f_cs_param_length = ProtoField.uint16("iap2.cs.param.length","Parameter Length",             base.DEC)
local f_cs_param_id     = ProtoField.uint16("iap2.cs.param.id",   "Parameter ID",                 base.HEX)
local f_cs_param_name   = ProtoField.string("iap2.cs.param.name", "[Parameter Name]")
local f_cs_param_data   = ProtoField.bytes ("iap2.cs.param.data", "Parameter Data")
local f_cs_param_str    = ProtoField.string("iap2.cs.param.str",  "Value (UTF-8)")
local f_cs_param_uint8  = ProtoField.uint8 ("iap2.cs.param.uint8","Value (uint8)",                base.DEC)
local f_cs_param_uint16 = ProtoField.uint16("iap2.cs.param.uint16","Value (uint16)",              base.DEC)
local f_cs_param_uint32 = ProtoField.uint32("iap2.cs.param.uint32","Value (uint32)",              base.DEC)
local f_cs_param_uint64 = ProtoField.uint64("iap2.cs.param.uint64","Value (uint64)",              base.DEC)
local f_cs_param_int8   = ProtoField.int8  ("iap2.cs.param.int8", "Value (int8)",                 base.DEC)
local f_cs_param_int16  = ProtoField.int16 ("iap2.cs.param.int16","Value (int16)",                base.DEC)
local f_cs_param_int32  = ProtoField.int32 ("iap2.cs.param.int32","Value (int32)",                base.DEC)
local f_cs_param_bool   = ProtoField.uint8 ("iap2.cs.param.bool", "Value (bool)",                 base.DEC)

-- ============================================================
-- File Transfer Session Fields
-- ============================================================

local f_ft_id           = ProtoField.uint8 ("iap2.ft.id",          "FileTransfer Identifier",      base.DEC)
local f_ft_type         = ProtoField.uint8 ("iap2.ft.type",        "Datagram Type",                base.HEX)
local f_ft_type_name    = ProtoField.string("iap2.ft.type_name",   "[Datagram Name]")
local f_ft_filesize     = ProtoField.uint64("iap2.ft.filesize",    "File Size",                    base.DEC)
local f_ft_filetype     = ProtoField.uint16("iap2.ft.filetype",    "File Type",                    base.HEX)
local f_ft_filetype_name= ProtoField.string("iap2.ft.filetype_name","[File Type Name]")
local f_ft_setup_data   = ProtoField.bytes ("iap2.ft.setup_data",  "File Type Setup Data")
local f_ft_payload      = ProtoField.bytes ("iap2.ft.payload",     "Transfer Data")

-- ============================================================
-- External Accessory Session Fields
-- ============================================================

local f_ea_sess_id      = ProtoField.uint16("iap2.ea.session_id",  "EA Session Identifier",        base.DEC)
local f_ea_data         = ProtoField.bytes ("iap2.ea.data",        "EA Session Data")

-- Register all fields
iap2_proto.fields = {
    f_link_sop, f_link_length, f_link_ctrl,
    f_link_ctrl_syn, f_link_ctrl_ack, f_link_ctrl_eak, f_link_ctrl_rst, f_link_ctrl_slp,
    f_link_seq, f_link_ack, f_link_session_id,
    f_link_hdr_cksum, f_link_hdr_cksum_status, f_link_pkt_type,
    f_syn_version, f_syn_max_out, f_syn_max_len, f_syn_retx_to,
    f_syn_cumack_to, f_syn_max_retx, f_syn_max_cumack, f_syn_link_mode,
    f_syn_sess_id, f_syn_sess_type, f_syn_sess_ver, f_syn_sess_type_str,
    f_eak_seq,
    f_payload_cksum, f_payload_cksum_status,
    f_cs_msg_sop, f_cs_msg_length, f_cs_msg_id, f_cs_msg_name, f_cs_msg_source,
    f_cs_param_length, f_cs_param_id, f_cs_param_name, f_cs_param_data,
    f_cs_param_str, f_cs_param_uint8, f_cs_param_uint16, f_cs_param_uint32,
    f_cs_param_uint64, f_cs_param_int8, f_cs_param_int16, f_cs_param_int32,
    f_cs_param_bool,
    f_ft_id, f_ft_type, f_ft_type_name, f_ft_filesize, f_ft_filetype,
    f_ft_filetype_name, f_ft_setup_data, f_ft_payload,
    f_ea_sess_id, f_ea_data,
}

-- ============================================================
-- Lookup tables
-- ============================================================

local function ctrl_to_pkt_type(ctrl)
    local syn = bit.band(ctrl, 0x80) ~= 0
    local ack = bit.band(ctrl, 0x40) ~= 0
    local eak = bit.band(ctrl, 0x20) ~= 0
    local rst = bit.band(ctrl, 0x10) ~= 0
    local slp = bit.band(ctrl, 0x08) ~= 0
    if rst then return "RST" end
    if slp then return "SLP" end
    if syn and ack then return "SYN+ACK" end
    if syn then return "SYN" end
    if eak then return "EAK" end
    if ack then return "ACK" end
    return "Unknown"
end

local SESSION_TYPE_NAMES = {
    [0] = "Control",
    [1] = "File Transfer",
    [2] = "External Accessory (EA)",
}

local CONTROL_MESSAGES = {
    [0xAA00] = { "RequestAuthenticationCertificate",           "Device"    },
    [0xAA01] = { "AuthenticationCertificate",                   "Accessory" },
    [0xAA02] = { "RequestAuthenticationChallengeResponse",     "Device"    },
    [0xAA03] = { "AuthenticationResponse",                     "Accessory" },
    [0xAA04] = { "AuthenticationFailed",                       "Device"    },
    [0xAA05] = { "AuthenticationSucceeded",                    "Device"    },
    [0xAA06] = { "AccessoryAuthenticationSerialNumber",        "Accessory" },
    [0xAA10] = { "RequestDeviceAuthenticationCertificate",     "Accessory" },
    [0xAA11] = { "DeviceAuthenticationCertificate",            "Device"    },
    [0xAA12] = { "RequestDeviceAuthenticationChallengeResponse","Accessory"},
    [0xAA13] = { "DeviceAuthenticationResponse",               "Device"    },
    [0xAA14] = { "DeviceAuthenticationFailed",                 "Accessory" },
    [0xAA15] = { "DeviceAuthenticationSucceeded",              "Accessory" },
    [0x1D00] = { "StartIdentification",                        "Device"    },
    [0x1D01] = { "IdentificationInformation",                  "Accessory" },
    [0x1D02] = { "IdentificationAccepted",                     "Device"    },
    [0x1D03] = { "IdentificationRejected",                     "Device"    },
    [0x1D05] = { "CancelIdentification",                       "Accessory" },
    [0x1D06] = { "IdentificationInformationUpdate",            "Accessory" },
    [0xEA00] = { "StartExternalAccessoryProtocolSession",       "Device"    },
    [0xEA01] = { "StopExternalAccessoryProtocolSession",        "Device"    },
    [0xEA02] = { "RequestAppLaunch",                           "Accessory" },
    [0xEA03] = { "StatusExternalAccessoryProtocolSession",     "Accessory" },
    [0xAD00] = { "StartAppDiscoveryUpdates",                   "Accessory" },
    [0xAD01] = { "AppDiscoveryUpdate",                         "Device"    },
    [0xAD02] = { "StopAppDiscoveryUpdates",                    "Accessory" },
    [0x5400] = { "StartAssistiveTouch",                        "Accessory" },
    [0x5401] = { "StopAssistiveTouch",                         "Accessory" },
    [0x5402] = { "StartAssistiveTouchInformation",             "Accessory" },
    [0x5403] = { "AssistiveTouchInformation",                   "Device"    },
    [0x5404] = { "StopAssistiveTouchInformation",              "Accessory" },
    [0x4E01] = { "BluetoothComponentInformation",              "Accessory" },
    [0x4E03] = { "StartBluetoothConnectionUpdates",            "Accessory" },
    [0x4E04] = { "BluetoothConnectionUpdate",                  "Device"    },
    [0x4E05] = { "StopBluetoothConnectionUpdates",             "Accessory" },
    [0x4E09] = { "DeviceInformationUpdate",                    "Device"    },
    [0x4E0A] = { "DeviceLanguageUpdate",                       "Device"    },
    [0x4E0B] = { "DeviceTimeUpdate",                           "Device"    },
    [0x4E0C] = { "DeviceUUIDUpdate",                           "Device"    },
    [0x4E0D] = { "WirelessCarPlayUpdate",                      "Device"    },
    [0x4E0E] = { "DeviceTransportIdentifierNotification",      "Device"    },
    [0x4154] = { "StartCallStateUpdates",                      "Accessory" },
    [0x4155] = { "CallStateUpdate",                             "Device"    },
    [0x4156] = { "StopCallStateUpdates",                       "Accessory" },
    [0x4157] = { "StartCommunicationsUpdates",                 "Accessory" },
    [0x4158] = { "CommunicationsUpdate",                       "Device"    },
    [0x4159] = { "StopCommunicationsUpdates",                  "Accessory" },
    [0x415A] = { "InitiateCall",                               "Accessory" },
    [0x415B] = { "AcceptCall",                                 "Accessory" },
    [0x415C] = { "EndCall",                                    "Accessory" },
    [0x415D] = { "SwapCalls",                                  "Accessory" },
    [0x415E] = { "MergeCalls",                                 "Accessory" },
    [0x415F] = { "HoldStatusUpdate",                           "Accessory" },
    [0x4160] = { "MuteStatusUpdate",                           "Accessory" },
    [0x4161] = { "SendDTMF",                                   "Accessory" },
    [0x4C00] = { "StartMediaLibraryInformation",               "Accessory" },
    [0x4C01] = { "MediaLibraryInformation",                    "Device"    },
    [0x4C02] = { "StopMediaLibraryInformation",                "Accessory" },
    [0x4C03] = { "StartMediaLibraryUpdates",                   "Accessory" },
    [0x4C04] = { "MediaLibraryUpdate",                         "Device"    },
    [0x4C05] = { "StopMediaLibraryUpdates",                    "Accessory" },
    [0x4C06] = { "PlayMediaLibraryCurrentSelection",           "Accessory" },
    [0x4C07] = { "PlayMediaLibraryItems",                       "Accessory" },
    [0x4C08] = { "PlayMediaLibraryCollection",                 "Accessory" },
    [0x4C09] = { "PlayMediaLibrarySpecial",                    "Accessory" },
    [0x5000] = { "StartNowPlayingUpdates",                     "Accessory" },
    [0x5001] = { "NowPlayingUpdate",                           "Device"    },
    [0x5002] = { "StopNowPlayingUpdates",                      "Accessory" },
    [0x5003] = { "SetNowPlayingInformation",                   "Accessory" },
    [0xAE00] = { "StartPowerUpdates",                          "Accessory" },
    [0xAE01] = { "PowerUpdate",                                "Device"    },
    [0xAE02] = { "StopPowerUpdates",                           "Accessory" },
    [0xAE03] = { "PowerSourceUpdate",                          "Accessory" },
    [0x6800] = { "StartHID",                                   "Accessory" },
    [0x6801] = { "DeviceHIDReport",                            "Device"    },
    [0x6802] = { "AccessoryHIDReport",                         "Accessory" },
    [0x6803] = { "StopHID",                                    "Accessory" },
    [0x6806] = { "StartNativeHID",                             "Device"    },
    [0xFFFA] = { "StartLocationInformation",                    "Device"    },
    [0xFFFB] = { "LocationInformation",                        "Accessory" },
    [0xFFFC] = { "StopLocationInformation",                    "Device"    },
    [0xDA00] = { "StartUSBDeviceModeAudio",                    "Accessory" },
    [0xDA01] = { "USBDeviceModeAudioInformation",              "Device"    },
    [0xDA02] = { "StopUSBDeviceModeAudio",                     "Accessory" },
    [0x5601] = { "RequestVoiceOverMoveCursor",                 "Accessory" },
    [0x5602] = { "RequestVoiceOverActivateCursor",             "Accessory" },
    [0x5603] = { "RequestVoiceOverScrollPage",                 "Accessory" },
    [0x5606] = { "RequestVoiceOverSpeakText",                  "Accessory" },
    [0x5608] = { "RequestVoiceOverPauseText",                  "Accessory" },
    [0x5609] = { "RequestVoiceOverResumeText",                 "Accessory" },
    [0x560B] = { "StartVoiceOverUpdates",                      "Accessory" },
    [0x560C] = { "VoiceOverUpdate",                            "Device"    },
    [0x560D] = { "StopVoiceOverUpdates",                       "Accessory" },
    [0x560E] = { "RequestVoiceOverConfiguration",              "Accessory" },
    [0x560F] = { "StartVoiceOverCursorUpdates",                "Accessory" },
    [0x5610] = { "VoiceOverCursorUpdate",                      "Device"    },
    [0x5611] = { "StopVoiceOverCursorUpdates",                 "Accessory" },
    [0x5612] = { "StartVoiceOver",                             "Accessory" },
    [0x5613] = { "StopVoiceOver",                              "Accessory" },
    [0x5700] = { "RequestWiFiInformation",                     "Accessory" },
    [0x5701] = { "WiFiInformation",                            "Device"    },
    [0x5702] = { "RequestAccessoryWiFiConfigurationInformation","Device"   },
    [0x5703] = { "AccessoryWiFiConfigurationInformation",      "Accessory" },
    [0x0B00] = { "StartBluetoothPairing",                      "Device"    },
    [0x0B01] = { "BluetoothPairingAccessoryInformation",       "Accessory" },
    [0x0B02] = { "BluetoothPairingStatus",                     "Accessory" },
    [0x0B03] = { "StopBluetoothPairing",                       "Device"    },
    [0xA100] = { "StartVehicleStatusUpdates",                  "Device"    },
    [0xA101] = { "VehicleStatusUpdate",                        "Accessory" },
    [0xA102] = { "StopVehicleStatusUpdates",                   "Device"    },
    [0x5200] = { "StartRouteGuidanceUpdates",                  "Accessory" },
    [0x5201] = { "RouteGuidanceUpdate",                        "Device"    },
    [0x5202] = { "RouteGuidanceManeuverUpdate",                "Device"    },
    [0x5203] = { "StopRouteGuidanceUpdates",                   "Accessory" },
}

local FT_DATAGRAM_NAMES = {
    [0x00] = "Data",
    [0x01] = "Start",
    [0x02] = "Cancel",
    [0x03] = "Pause",
    [0x04] = "Setup",
    [0x05] = "Success",
    [0x06] = "Failure",
    [0x40] = "LastData",
    [0x80] = "FirstData",
    [0xC0] = "FirstAndOnlyData",
}

local FT_FILE_TYPE_NAMES = {
    [0x0000] = "Reserved",
    [0x0001] = "CallStateUpdateVCard",
    [0x0002] = "NowPlayingArtworkData",
    [0x0003] = "NowPlayingPlaybackQueueContents",
    [0x0004] = "MediaLibraryUpdatePlaylistContents",
    [0x0006] = "MediaItemListNowPlayingPlaybackQueueContents",
    [0x0007] = "MediaItemListMediaLibraryUpdatePlaylistContents",
    [0x0008] = "AppDiscoveryIconData",
}

-- ============================================================
-- Checksum helper
-- ============================================================

local function checksum_ok(tvb, offset, length, stored)
    local total = 0
    for i = offset, offset + length - 1 do
        total = total + tvb:range(i, 1):uint()
    end
    total = total + stored
    return bit.band(total, 0xFF) == 0
end

-- ============================================================
-- Control Session dissector
-- ============================================================

local function dissect_control_session(tvb, pinfo, tree, offset, length)
    local end_offset = offset + length
    while offset < end_offset do
        if offset + 6 > end_offset then break end

        local msg_start = offset
        local som       = tvb:range(offset, 2):uint()

        if som ~= 0x4040 then
            tree:add(iap2_proto, tvb:range(offset, end_offset - offset),
                "[Unrecognized control session data]")
            break
        end

        local msg_len   = tvb:range(offset + 2, 2):uint()
        local msg_id    = tvb:range(offset + 4, 2):uint()

        if msg_len < 6 or offset + msg_len > end_offset then break end

        local msg_info  = CONTROL_MESSAGES[msg_id]
        local msg_name  = msg_info and msg_info[1] or string.format("Unknown (0x%04X)", msg_id)
        local msg_src   = msg_info and msg_info[2] or "?"

        local msg_tree = tree:add(iap2_proto, tvb:range(offset, msg_len),
            string.format("Message: %s (0x%04X)", msg_name, msg_id))

        msg_tree:add(f_cs_msg_sop,    tvb:range(offset,     2))
        msg_tree:add(f_cs_msg_length, tvb:range(offset + 2, 2))
        msg_tree:add(f_cs_msg_id,     tvb:range(offset + 4, 2))
        msg_tree:add(f_cs_msg_name,   msg_name)
        msg_tree:add(f_cs_msg_source, msg_src)

        local param_offset = offset + 6
        local param_end    = offset + msg_len
        while param_offset < param_end do
            if param_offset + 4 > param_end then break end

            local p_len  = tvb:range(param_offset,     2):uint()
            local p_id   = tvb:range(param_offset + 2, 2):uint()

            if p_len < 4 or param_offset + p_len > param_end then break end

            local p_data_len = p_len - 4
            local param_tree = msg_tree:add(iap2_proto,
                tvb:range(param_offset, p_len),
                string.format("Parameter 0x%04X (%d bytes)", p_id, p_data_len))

            param_tree:add(f_cs_param_length, tvb:range(param_offset,     2))
            param_tree:add(f_cs_param_id,     tvb:range(param_offset + 2, 2))

            if p_data_len > 0 then
                local data_off = param_offset + 4
                if p_data_len == 1 then
                    param_tree:add(f_cs_param_uint8, tvb:range(data_off, 1))
                elseif p_data_len == 2 then
                    param_tree:add(f_cs_param_uint16, tvb:range(data_off, 2))
                elseif p_data_len == 4 then
                    param_tree:add(f_cs_param_uint32, tvb:range(data_off, 4))
                elseif p_data_len == 8 then
                    param_tree:add(f_cs_param_uint64, tvb:range(data_off, 8))
                else
                    local raw = tvb:range(data_off, p_data_len):bytes()
                    local is_printable = true
                    for i = 0, p_data_len - 1 do
                        local b = raw:get_index(i)
                        if b == 0 then
                            break
                        end
                        if b < 0x20 and b ~= 0x09 and b ~= 0x0A and b ~= 0x0D then
                            is_printable = false
                            break
                        end
                    end
                    if is_printable then
                        param_tree:add(f_cs_param_str, tvb:range(data_off, p_data_len))
                    else
                        param_tree:add(f_cs_param_data, tvb:range(data_off, p_data_len))
                    end
                end
            end

            param_offset = param_offset + p_len
        end

        offset = offset + msg_len
    end
end

-- ============================================================
-- File Transfer Session dissector
-- ============================================================

local function dissect_file_transfer_session(tvb, pinfo, tree, offset, length)
    if length < 2 then
        tree:add(iap2_proto, tvb:range(offset, length), "[File Transfer: too short]")
        return
    end

    local ft_id       = tvb:range(offset, 1):uint()
    local ft_type_val = tvb:range(offset + 1, 1):uint()
    local type_name   = FT_DATAGRAM_NAMES[ft_type_val]
                        or string.format("Unknown (0x%02X)", ft_type_val)

    local ft_tree = tree:add(iap2_proto, tvb:range(offset, length),
        string.format("File Transfer: %s (ID %d)", type_name, ft_id))

    ft_tree:add(f_ft_id,        tvb:range(offset,     1))
    ft_tree:add(f_ft_type,      tvb:range(offset + 1, 1))
    ft_tree:add(f_ft_type_name, type_name)

    if ft_type_val == 0x04 and length >= 10 then
        ft_tree:add(f_ft_filesize, tvb:range(offset + 2, 8))

        if length >= 12 then
            local file_type_val  = tvb:range(offset + 10, 2):uint()
            local file_type_name = FT_FILE_TYPE_NAMES[file_type_val]
                                   or string.format("Unknown (0x%04X)", file_type_val)
            ft_tree:add(f_ft_filetype,      tvb:range(offset + 10, 2))
            ft_tree:add(f_ft_filetype_name, file_type_name)
            if length > 12 then
                ft_tree:add(f_ft_setup_data, tvb:range(offset + 12, length - 12))
            end
        end

    elseif (ft_type_val == 0x00 or ft_type_val == 0x40 or
            ft_type_val == 0x80 or ft_type_val == 0xC0) and length > 2 then
        ft_tree:add(f_ft_payload, tvb:range(offset + 2, length - 2))
    end
end

-- ============================================================
-- External Accessory Session dissector
-- ============================================================

local function dissect_ea_session(tvb, pinfo, tree, offset, length)
    if length < 2 then
        tree:add(iap2_proto, tvb:range(offset, length), "[EA Session: too short]")
        return
    end

    local ea_sess_id = tvb:range(offset, 2):uint()
    local ea_tree = tree:add(iap2_proto, tvb:range(offset, length),
        string.format("EA Session: ID %d (%d bytes data)", ea_sess_id, length - 2))

    ea_tree:add(f_ea_sess_id, tvb:range(offset, 2))
    if length > 2 then
        ea_tree:add(f_ea_data, tvb:range(offset + 2, length - 2))
    end
end

-- ============================================================
-- Link Synchronization Payload dissector
-- ============================================================

local function dissect_syn_payload(tvb, pinfo, tree, offset, length)
    local syn_tree = tree:add(iap2_proto, tvb:range(offset, length),
        "Link Synchronization Payload")

    if length < 10 then
        syn_tree:add(iap2_proto, tvb:range(offset, length), "[SYN payload too short]")
        return
    end

    syn_tree:add(f_syn_version,    tvb:range(offset,     1))
    syn_tree:add(f_syn_max_out,    tvb:range(offset + 1, 1))
    syn_tree:add(f_syn_max_len,    tvb:range(offset + 2, 2))
    syn_tree:add(f_syn_retx_to,    tvb:range(offset + 4, 2))
    syn_tree:add(f_syn_cumack_to,  tvb:range(offset + 6, 2))
    syn_tree:add(f_syn_max_retx,   tvb:range(offset + 8, 1))
    syn_tree:add(f_syn_max_cumack, tvb:range(offset + 9, 1))

    local retx_to   = tvb:range(offset + 4, 2):uint()
    local cumack_to = tvb:range(offset + 6, 2):uint()
    local max_retx  = tvb:range(offset + 8, 1):uint()
    local max_cumack= tvb:range(offset + 9, 1):uint()
    if retx_to == 0 and cumack_to == 0 and max_retx == 0 and max_cumack == 0 then
        syn_tree:add(f_syn_link_mode, "[ZeroACK / ZeroRetransmit mode]")
    else
        syn_tree:add(f_syn_link_mode, "[Standard ACK mode]")
    end

    local sess_off = offset + 10
    local sess_end = offset + length
    local sess_num = 1
    while sess_off + 3 <= sess_end do
        local s_id   = tvb:range(sess_off,     1):uint()
        local s_type = tvb:range(sess_off + 1, 1):uint()
        local s_ver  = tvb:range(sess_off + 2, 1):uint()
        local s_type_name = SESSION_TYPE_NAMES[s_type]
                            or string.format("Unknown (%d)", s_type)

        local sess_tree = syn_tree:add(iap2_proto, tvb:range(sess_off, 3),
            string.format("Session %d: ID=%d Type=%s Ver=%d",
                sess_num, s_id, s_type_name, s_ver))
        sess_tree:add(f_syn_sess_id,       tvb:range(sess_off,     1))
        sess_tree:add(f_syn_sess_type,     tvb:range(sess_off + 1, 1))
        sess_tree:add(f_syn_sess_type_str, s_type_name)
        sess_tree:add(f_syn_sess_ver,      tvb:range(sess_off + 2, 1))

        sess_off = sess_off + 3
        sess_num = sess_num + 1
    end
end

-- ============================================================
-- Extended Acknowledgement Payload dissector
-- ============================================================

local function dissect_eak_payload(tvb, pinfo, tree, offset, length)
    local eak_tree = tree:add(iap2_proto, tvb:range(offset, length),
        "Extended Acknowledgement Payload")
    for i = 0, length - 1 do
        eak_tree:add(f_eak_seq, tvb:range(offset + i, 1))
    end
end

-- ============================================================
-- Session type state
-- ============================================================

local session_type_map = {}

local function get_session_type(pinfo, session_id)
    local conv = tostring(pinfo.net_src) .. "-" .. tostring(pinfo.net_dst)
    if session_type_map[conv] then
        return session_type_map[conv][session_id]
    end
    return nil
end

local function store_session_types(pinfo, sessions)
    local conv = tostring(pinfo.net_src) .. "-" .. tostring(pinfo.net_dst)
    if not session_type_map[conv] then
        session_type_map[conv] = {}
    end
    for _, s in ipairs(sessions) do
        session_type_map[conv][s.id] = s.sess_type
    end
end

-- ============================================================
-- Info-column helpers
-- ============================================================

local SESSION_TAG = { [0] = "[CS]", [1] = "[FT]", [2] = "[EA]" }

local function summarise_control_messages(tvb, offset, length)
    local names = {}
    local end_offset = offset + length
    while offset + 6 <= end_offset do
        if tvb:range(offset, 2):uint() ~= 0x4040 then break end
        local msg_len = tvb:range(offset + 2, 2):uint()
        local msg_id  = tvb:range(offset + 4, 2):uint()
        if msg_len < 6 or offset + msg_len > end_offset then break end
        local info = CONTROL_MESSAGES[msg_id]
        if info then
            table.insert(names, info[1])
        else
            table.insert(names, string.format("Unknown (0x%04X)", msg_id))
        end
        offset = offset + msg_len
    end
    return #names > 0 and table.concat(names, ", ") or nil
end

local function summarise_ft_datagram(tvb, offset, length)
    if length < 2 then return nil end
    local ft_id   = tvb:range(offset, 1):uint()
    local ft_type = tvb:range(offset + 1, 1):uint()
    local name    = FT_DATAGRAM_NAMES[ft_type]
                    or string.format("Unknown (0x%02X)", ft_type)
    return string.format("%s (ID %d)", name, ft_id)
end

-- ============================================================
-- Main dissector (Expects clean, bounded iAP2 packet frame)
-- ============================================================

iap2_written_frame = -1
iap2_written_frame_info = ""

function iap2_proto.dissector(tvb, pinfo, tree)
    local length = tvb:len()
    if length < 9 then return 0 end

    local offset    = 0
    local pkt_count = 0
    local info_parts = {}

    while offset + 9 <= length do
        local sop = tvb:range(offset, 2):uint()
        if sop ~= 0xFF5A then break end

        local pkt_len = tvb:range(offset + 2, 2):uint()
        if pkt_len < 9 or offset + pkt_len > length then break end

        pkt_count = pkt_count + 1

        local ctrl       = tvb:range(offset + 4, 1):uint()
        local seq        = tvb:range(offset + 5, 1):uint()
        local ack_num    = tvb:range(offset + 6, 1):uint()
        local session_id = tvb:range(offset + 7, 1):uint()
        local hdr_cksum  = tvb:range(offset + 8, 1):uint()

        local pkt_type    = ctrl_to_pkt_type(ctrl)
        local has_payload = pkt_len > 9

        local pkt_label
        if has_payload then
            pkt_label = string.format("iAP2 Link %s  seq=%d ack=%d  sid=%d  [%d bytes payload]",
                pkt_type, seq, ack_num, session_id, pkt_len - 9 - 1)
        else
            pkt_label = string.format("iAP2 Link %s  seq=%d ack=%d", pkt_type, seq, ack_num)
        end

        local link_tree = tree:add(iap2_proto, tvb:range(offset, pkt_len), pkt_label)

        link_tree:add(f_link_sop,    tvb:range(offset,     2))
        link_tree:add(f_link_length, tvb:range(offset + 2, 2))

        local ctrl_tree = link_tree:add(f_link_ctrl, tvb:range(offset + 4, 1))
        ctrl_tree:add(f_link_ctrl_syn, tvb:range(offset + 4, 1))
        ctrl_tree:add(f_link_ctrl_ack, tvb:range(offset + 4, 1))
        ctrl_tree:add(f_link_ctrl_eak, tvb:range(offset + 4, 1))
        ctrl_tree:add(f_link_ctrl_rst, tvb:range(offset + 4, 1))
        ctrl_tree:add(f_link_ctrl_slp, tvb:range(offset + 4, 1))

        link_tree:add(f_link_seq,        tvb:range(offset + 5, 1))
        link_tree:add(f_link_ack,        tvb:range(offset + 6, 1))
        link_tree:add(f_link_session_id, tvb:range(offset + 7, 1))
        link_tree:add(f_link_hdr_cksum,  tvb:range(offset + 8, 1))

        local hdr_ok = checksum_ok(tvb, offset, 8, hdr_cksum)
        link_tree:add(f_link_hdr_cksum_status,
            hdr_ok and "[Header Checksum: OK]" or "[Header Checksum: BAD]")

        link_tree:add(f_link_pkt_type, string.format("[Packet Type: %s]", pkt_type))

        if has_payload then
            local payload_start     = offset + 9
            local payload_len       = pkt_len - 9 - 1
            local payload_cksum_off = offset + pkt_len - 1
            local payload_cksum     = tvb:range(payload_cksum_off, 1):uint()

            local pay_ok   = checksum_ok(tvb, payload_start, payload_len, payload_cksum)
            local syn_flag = bit.band(ctrl, 0x80) ~= 0
            local eak_flag = bit.band(ctrl, 0x20) ~= 0
            local ack_flag = bit.band(ctrl, 0x40) ~= 0

            if syn_flag then
                local sessions = {}
                if payload_len >= 10 then
                    local sess_off = payload_start + 10
                    local sess_end = payload_start + payload_len
                    while sess_off + 3 <= sess_end do
                        table.insert(sessions, {
                            id        = tvb:range(sess_off, 1):uint(),
                            sess_type = tvb:range(sess_off + 1, 1):uint(),
                        })
                        sess_off = sess_off + 3
                    end
                end
                if not pinfo.visited then
                    store_session_types(pinfo, sessions)
                end
                dissect_syn_payload(tvb, pinfo, link_tree, payload_start, payload_len)
                table.insert(info_parts, pkt_type)

            elseif eak_flag then
                dissect_eak_payload(tvb, pinfo, link_tree, payload_start, payload_len)
                table.insert(info_parts, "EAK")

            elseif ack_flag and session_id ~= 0 then
                local sess_type = get_session_type(pinfo, session_id)
                local sess_tag  = (sess_type ~= nil and SESSION_TAG[sess_type])
                                  or string.format("[S%d]", session_id)
                local sess_type_name = sess_type ~= nil
                    and (SESSION_TYPE_NAMES[sess_type]
                         or string.format("Unknown Type %d", sess_type))
                    or "Unknown [no SYN seen]"

                local sess_label = string.format(
                    "[Session %d: %s] (%d bytes)", session_id, sess_type_name, payload_len)
                local sess_tree = link_tree:add(iap2_proto,
                    tvb:range(payload_start, payload_len), sess_label)

                local summary = nil
                if sess_type == 0 then
                    dissect_control_session(tvb, pinfo, sess_tree,
                        payload_start, payload_len)
                    summary = summarise_control_messages(tvb, payload_start, payload_len)
                elseif sess_type == 1 then
                    dissect_file_transfer_session(tvb, pinfo, sess_tree,
                        payload_start, payload_len)
                    summary = summarise_ft_datagram(tvb, payload_start, payload_len)
                elseif sess_type == 2 then
                    dissect_ea_session(tvb, pinfo, sess_tree,
                        payload_start, payload_len)
                    summary = string.format("EA data (%d bytes)", payload_len - 2)
                else
                    sess_tree:add(iap2_proto,
                        tvb:range(payload_start, payload_len),
                        "[Unknown session type — raw data]")
                end

                if summary then
                    table.insert(info_parts, sess_tag .. " " .. summary)
                else
                    table.insert(info_parts, sess_tag)
                end
            else
                link_tree:add(iap2_proto,
                    tvb:range(payload_start, payload_len),
                    string.format("[Payload: %d bytes]", payload_len))
                table.insert(info_parts, pkt_type)
            end

            link_tree:add(f_payload_cksum, tvb:range(payload_cksum_off, 1))
            link_tree:add(f_payload_cksum_status,
                pay_ok and "[Payload Checksum: OK]" or "[Payload Checksum: BAD]")
        else
            table.insert(info_parts, pkt_type)
        end

        offset = offset + pkt_len
    end

    if pkt_count == 0 then return 0 end

    pinfo.cols.protocol:set("iAP2")
    
    local summary = table.concat(info_parts, "  |  ")
    if iap2_written_frame == pinfo.number then
        iap2_written_frame_info = iap2_written_frame_info .. ", " .. summary
        pinfo.cols.info:set(iap2_written_frame_info)
    else
        iap2_written_frame = pinfo.number
        iap2_written_frame_info = summary
        pinfo.cols.info:set(summary)
    end

    return offset
end

-- ============================================================
-- Postdissector Integration for RFCOMM / Bluetooth
-- ============================================================

local f_frame_protocols = Field.new("frame.protocols")
local f_rfcomm_len = Field.new("btrfcomm.len")

local iap2_post_proto = Proto("iap2_post", "iAP2 Bluetooth Wrapper")

function iap2_post_proto.dissector(tvb, pinfo, tree)
    -- 1. Exit early if this frame does not contain an RFCOMM channel layer
    local protocols = f_frame_protocols()
    if not protocols or not string.find(protocols.value, "btrfcomm") then 
        return 
    end

    -- 2. Pull the field data out of the parsed RFCOMM headers
    local rfcomm_len_field = f_rfcomm_len()
    if not rfcomm_len_field then return end

    local payload_length = rfcomm_len_field.value
    local total_packet_len = tvb:reported_len()
    
    -- Strip the 1-byte trailing RFCOMM FCS checksum at the very end
    local payload_start_offset = total_packet_len - payload_length - 1

    -- 2 instead of 9 to also match handshake packets
    if payload_start_offset < 0 or payload_length < 2 then return end

    -- Slicing the target payload array
    local iap2_tvb = tvb(payload_start_offset, payload_length):tvb()

    -- 3. Check for specific Link Initialization / iAP Handshake pattern
    if payload_length >= 6 and iap2_tvb(0, 6):bytes():tohex() == "FF550200EE10" then
    	print(match)
        pinfo.cols.protocol:set("iAP")
        pinfo.cols.info:set("iAP Link Initialization Handshake")
        tree:add(iap2_post_proto, iap2_tvb(0, 6), "iAP Link Initialization Handshake")
        return
    end

    -- 4. Normal iAP2 link check sequence (0xFF5A)
    if payload_length < 9 then return end
    local check_sop = iap2_tvb(0, 2):uint()
    if check_sop ~= 0xFF5A then return end

    -- 4. Proxy the isolated slice directly into your main engine
    iap2_proto.dissector(iap2_tvb, pinfo, tree)
end

register_postdissector(iap2_post_proto)
