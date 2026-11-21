# CAN Bench – mechanics sheet

Sources:
- **FW** = CANable 2.5 firmware source (github.com/Elmue/CANable-2.5-firmware-Slcan-and-Candlelight), files named below.
- **MAN** = "User & Developer Manual.htm" in that repository (same text as netcult.ch/elmue/CANable Firmware Update).
- **DFUP** = `DFU Firmware Programmer.exe` shipped with HUD ECU Hacker (native, decompiled; addresses below).
- **ECU** = `ECU_Hacker.exe` 6.1.0.3. It is obfuscated (unicode names, encrypted strings), so its CAN Raw Terminal
  code could not be read; the terminal behaviour below comes from MAN screenshots/text of that terminal.
- **est.** = my own choice, looked for in the code and not found.

## ECU_Hacker.exe – what its own code does (decompiled from IL)
The program is .NET with control-flow obfuscation, unicode names and encrypted strings, so Ghidra/native pseudocode
sees only the `_CorExeMain` stub (0xa242ae). The code was read as IL (managed_members_list with IL, evidence
ev_ca6e1982…): numeric constants and call targets survive the obfuscation. Addresses are .NET method tokens.

| Rule | Value | Where |
|---|---|---|
| WinUSB control transfer wrapper (dir, type, recipient, request, wValue, wIndex, data) | IN = 0x80 | 0x060023b9 → WinUsb_ControlTransfer 0x060023a8 |
| HUD's Candlelight driver class | type 0x0200025e | uses 0x060023b9 through 0x06002147 |
| Request after a failed request = GetLastError | 22 | 0x06002147 |
| Open: Reset = SetDeviceMode {0, 0x4000} | request 2 | 0x06002144 IL_0d57, also Close 0x06002146 |
| Open reads GetCapabilities (4), GetCapabilitiesFD (11), GetDeviceVersion (5), GetBoardInfo (20) | IN requests | 0x06002144 |
| Open sets SetBitTiming (1), SetBitTimingFD (10), Start = SetDeviceMode {1, flags} | OUT requests | 0x06002144 |
| Flags HUD sets: 0x4000 ElmüSoft, 0x8000 blobs, 0x100 FD, 0x1000 error reports (if the adapter has it), 0x8 one-shot, 0x1 listen-only | | 0x06002144 |
| A flag is only set if the adapter's capability `feature` has it, otherwise an error | | 0x06002145 |
| Busload report interval sent on open | byte 10 (= 1 s) | 0x06002144 IL_0a61 → 0x0600214a (request 23) |
| Firmware date checks on open | ≤ 0x260113 (13 Jan 2026) → update required message; < 0x261001 (1 Oct 2026) → newer firmware message | 0x06002144 IL_0e70 / IL_075d |
| USB IN buffer | 2048 bytes; pipe timeout 500 ms | 0x06002144 |
| CAN FD supported = feature & 0x100 and feature & 0x400 | | 0x06002141 |
| ElmüSoft = feature & 0x4000 | | 0x06002142 |
| Identify needs feature & 0x20, then request 7 with u32 1/0 | | 0x06002148 |
| Hardware timestamp needs feature & 0x10, request 6 | | 0x06002149 (not used here: host timestamps) |
| Disable BOOT0 = request 24 {op 5, pin 1} | | 0x0600214b |
| BOOT0 status = request 25, wValue 1, bit 0x2 = enabled | | 0x0600214c |
| Free Tx slots = 64 − (txMarker − echoMarker), wrapping with +255 | 64 | 0x0600214e |
| Legacy Slcan S/Y baud table: 10k,20k,50k,100k,125k,250k,500k,800k,1M,83.333k; data 2M/5M | | 0x0600177e |
| Bit timing is stored by 0x060009f5 (fields 0x040005dd / 0x040005de) and sent by the open routine | | traced from 0x06002144 |
| Timing search driver: requested samplepoint in ‰ must be ≥ 500, nominal ≤ 900, data ≤ 800; loop brp = 1 … min(nominal brp max, data brp max); keep a candidate only if it is better; start error 9999999 | | 0x060009f1 |
| Per prescaler: tq = clock / brp / baud (integer); tq ≥ 3 and ≤ 1 + seg1max + seg2max; seg1+1 = tq × sp‰ / 1000 (integer); seg2 = tq − (seg1+1), clamped to seg2max; seg1 ≥ 1, seg2 ≥ 1, seg1 ≤ seg1max; baud error must be ≤ 0.5 % | | 0x06000a12 |
| tq = 1 + seg1 + seg2; minimum tq 3 | | 0x06000a09/0a/0b, 0x06000a08 |
| actual samplepoint ‰ = 1000 × (1 + seg1) / tq; actual baud = clock / brp / tq (integers) | | 0x06000a0c, 0x06000a0d |
| error % = abs(actual − wanted) / wanted × 100 (baud and samplepoint) | | 0x06000a0e, 0x06000a0f |
| Perfect match = both errors < 0.01 % | | 0x06000a10 |
| Compare: lower baud error wins; if equal, lower samplepoint error wins | | 0x06000a11 |
| A further check against 880 ‰ in 0x06000a12 could not be placed (it would reject every request above 88 %, but 90 % is allowed) — **not implemented** | | 0x06000a12 IL_023e |
| SJW: the min/max call in 0x06000a12 goes through an unnamed BCL reference; the build uses min(seg1, seg2) as the firmware sample does (**estimated**) | | 0x06000a12 IL_0267 |

## USB identities
| What | Value | Source |
|---|---|---|
| Candlelight 2.5 VID:PID | 1D50:606F | MAN screenshot "CAN Raw Terminal" |
| Slcan 2.5 VID:PID | 16D0:117E (USB CDC COM port) | MAN screenshot "RS232 Terminal" |
| STM32 ROM bootloader (DFU mode) | 0483:DF11 | DfuSe standard, DFUP string "STM32 Device in DFU Mode" (0x5635ec) |
| Candlelight interfaces | 0 = CAN ch1, 1 = DFU (run-time), 2 = ch2, 3 = ch3 | FW usb_class.h, MAN "Multi-Channel" |
| Candlelight endpoints | ch1: IN 0x81 / OUT 0x02; ch2: 0x83/0x04; ch3: 0x85/0x06 | MAN |
| USB blob size | 2048 bytes | FW Candlelight_def.h MAX_BLOB_SIZE |

## Candlelight control requests (vendor, recipient interface, wValue = channel, wIndex = interface)
GS: 0 SetHostFormat, 1 SetBitTiming, 2 SetDeviceMode, 4 GetCapabilities (40 B), 5 GetDeviceVersion (12 B),
7 Identify (u32 1/0), 10 SetBitTimingFD, 11 GetCapabilitiesFD (72 B), 12 SetTermination, 13 GetTermination.
ELM: 20 GetBoardInfo (56 B), 21 SetFilter (17 B packed), 22 GetLastError (1 B), 23 SetBusLoadReport (u8 interval×100 ms),
24 SetPinStatus (12 B), 25 GetPinStatus (wValue = pin, 2 B). — FW Candlelight_def.h eUsbRequest.
After any failed request, request 22 returns the feedback code (2 = success, '1'..'=' errors). — FW / sample Candlelight.cpp CtrlTransfer.

Device flags: ListenOnly 0x1, Loopback 0x2, OneShot 0x8, Timestamp 0x10, Identify 0x20, CAN_FD 0x100,
BitTimingFD 0x400, Termination 0x800, ElmueProtocol 0x4000, SendUsbBlobs 0x8000. Internal loopback = ListenOnly|Loopback.

Open sequence (sample Candlelight.cpp Open/Start): Reset (mode 0, flags 0x4000) → GetCapabilities → GetCapabilitiesFD
→ GetDeviceVersion → GetBoardInfo → GetPinStatus(BOOT0) → SetBitTiming {prop 0, seg1, seg2, sjw=min(seg1,seg2), brp}
→ (SetBitTimingFD) → filters → SetDeviceMode {mode 1, flags | 0x4000 | 0x8000 if supported}.

ElmüSoft messages (2-byte header size,type): 10 TxFrame {flags, can_id, marker}+data; 11 TxEcho {marker};
12 RxFrame {flags, can_id}+data; 13 Error {err_id u32, data[8]}; 14 String; 15 Busload {u8 %};
16 TxBlob / 17 RxBlob {count, type} + frames. CAN ID flags: 29-bit 0x80000000, RTR 0x40000000.
Frame flags: FDF 0x02, BRS 0x04, ESI 0x08. Tx marker 1..255, 0 = no echo.
Error decoding (Candlelight.cpp FormatCanErrors): err_id 0x40 Bus off, 0x20 No ACK, 0x200 CRC; data[1] 0x30 passive,
0x0C warning, 0x40 back to active; data[2] 0x04 stuffing, 0x02 form, 0x08 dominant, 0x10 recessive;
data[5] app flags 0x01 Rx fail, 0x02 Tx fail, 0x04 Tx overflow, 0x08 USB IN overflow, 0x10 Tx timeout; data[6] Tx errs, data[7] Rx errs.

DFU detach (enter boot mode): class request 0 (DFU_DETACH) to interface 1, then GETSTATUS (3, 6 bytes):
state 1 AppDetach → user must replug USB; state 10 Error → iString holds feedback code; otherwise device enters
the bootloader after 300 ms. — FW dfu.c, Candlelight.cpp EnterDfuMode.

Disable BOOT0 (DFUP sub_4029a4): SetDeviceMode {0, 0x4000}, GetCapabilities, require feature&0x4000,
then SetPinStatus {op 5 = Disable, pin 1 = BOOT0}.

## Slcan (ASCII, every command ends in CR) — MAN "Slcan Developer Manual"
Init: "\rC\r" (no feedback), "V\r" → "+Board:..\tMCU:..\tDevID:..\tFirmware:..\tSlcan:..\tClock:..\tChannels:..\tQuartz:..\tLimits:a,b,c,d,e,f,g,h\t..."
(≥7 parts = CANable 2.5), "MF\r" (feedback on) then wait "#\r" after each command.
Modes "M" + letters: A/a auto-retransmit, D debug, E errors, F feedback, I identify, M echo marker, R termination, S ESI.
Bitrate "s{brp},{seg1},{seg2},{sjw}\r", data "y…\r". Legacy S0..S8 = 10k,20k,50k,100k,125k,250k,500k,800k,1M.
Filters "F{id},{mask};…\r", clear "f\r". Busload "L{n}\r" (n×100 ms). Open "ON/OS/OI/OE\r", close "C\r".
Frames: t/T (11/29 bit), r/R RTR, d/D FD, b/B FD+BRS, ID 3 or 8 hex, DLC 1 hex, data, optional 2-hex marker.
Events: "#x" feedback, ">text" debug, "Exxxxxxxx" error (bus status, last protocol error, app flags, Tx, Rx), "Lnn" busload, "Mxx" echo.
Special: "*DFU\r" enter boot mode, "*Boot0:Off\r" disable BOOT0, "*Boot0:?\r" → "+1"/"+0".
DFUP sends "MF\r" before "*Boot0:Off\r" (sub_40220f, strings at 0x562c40 / 0x562c8c) and maps feedback chars via sub_402b7a.

Feedback codes (FW eFeedback): '1' invalid command, '2' invalid parameter, '3' must be open, '4' must be closed, '5' HAL error,
'6' unsupported, '7' Tx buffer full, '8' bus off, '9' no Tx in silent mode, ':' baudrate not set, ';' option bytes failed,
'<' reconnect USB (reset required), '=' parameter out of range.

## DfuSe flashing (ST AN3156 / UM0391, as used by DFUP's CDownloadThread)
Requests (class, interface 0): 1 DNLOAD, 2 UPLOAD, 3 GETSTATUS (6 B), 4 CLRSTATUS, 5 GETSTATE, 6 ABORT.
Commands sent as DNLOAD block 0: 0x21+addr = set address pointer, 0x41+addr = erase page, 0x92 = read unprotect.
Data blocks: DNLOAD wValue = 2+n, address = pointer + n × transfer size. Transfer size = 2048 (STM32 bootloader, est. if the
functional descriptor isn't readable). Flash page = 2 KB on STM32G431/G473(dual bank)/G0B1 (MAN "Read/Write Flash Memory").
Leave DFU: set address 0x08000000, DNLOAD length 0, GETSTATUS → device restarts (DFUP "Leave DFU mode").
States: 2 idle, 3 dnload-sync, 4 dnbusy, 5 dnload-idle, 6 manifest-sync, 7 manifest, 8 manifest-wait-reset, 9 upload-idle, 10 error.
DfuSe file: "DfuSe" v1 prefix (11 B), per target "Target" prefix (274 B) + elements {addr, size, data}, suffix 16 B
(bcdDevice, PID, VID, bcdDFU 0x011A, "UFD", 16, CRC32 without final xor).

## Bit timing (MAN "Samplepoint & Baudrate")
baud = clock / brp / (1 + seg1 + seg2); samplepoint = (1 + seg1) / (1 + seg1 + seg2); sjw = min(seg1, seg2).
CANable 2.5 uses a 160 MHz CAN clock on STM32G4 (60 MHz on G0B1). The search is HUD's own (0x060009f1 / 0x06000a12, see the
table above): shared prescaler, ascending, baud error first then samplepoint error, stop at a perfect match. Check: it reproduces
the manual's example exactly (500k 87.5 % / 2M 75 % at 160 MHz → prescaler 2, seg1 139, seg2 20 / prescaler 2, seg1 29, seg2 10).
Default terminal values: 500 kbaud 87.5 %, data 2 Mbaud 75 % (MAN example output) — **est.** as defaults.

## Terminal behaviour (MAN "CAN bus Settings", screenshot "CAN Raw Terminal")
Colours: blue = sent, green = received, olive = Tx echo (ACK), magenta = adapter info, red = errors.
Options: pad to 8 bytes, show all packets, auto retransmission (off = one-shot), show echo, suppress repeated identical packets.
Packet text: "18EEFF01: 01 02 03 … (FDF, BRS)". Busload reported once per second when > 0.
Error repeat rules are the firmware's (first immediately, next after 100 ms, same error every 3 s).
