# UBC125XLT Scanner Commands

This document serves as a technical reference for the serial commands supported by the UBC125XLT / BC125AT scanner.

## Command Format

*   **Controller -> Radio:** `<COMMAND> [,<PARAMETERS>] \r`
*   **Radio -> Controller:** `<COMMAND> [,<RESPONSE_DATA>] \r`
*   **Error Response:** `ERR\r` (Format/Value error) or `NG\r` (Invalid at this time)

## Official Commands

These commands are documented in the BC125AT Operation Specification.

| Command | Description | Mode | Controller Format (Get/Action) | Controller Format (Set) | Notes |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **PRG** | Enter Program Mode | All | `PRG` | - | Scanner displays "Remote Mode". Required for most memory commands. |
| **EPG** | Exit Program Mode | Prg | `EPG` | - | Returns to Scan Hold Mode. |
| **MDL** | Get Model Info | All | `MDL` | - | Returns `MDL,BC125AT`. |
| **VER** | Get Firmware Version | All | `VER` | - | Returns `VER,Version X.XX.XX`. |
| **BLT** | Get/Set Backlight | Prg | `BLT` | `BLT,[EVNT]` | `EVNT`: AO(Always On), AF(Always Off), KY(Keypress), KS(Key+SQL) |
| **BSV** | Get/Set Battery Info | Prg | `BSV` | `BSV,[CHARGE_TIME]` | `CHARGE_TIME`: 1-16 (hours). |
| **CLR** | Clear All Memory | Prg | `CLR` | - | Resets all memories to initial setting. Takes time. |
| **KBP** | Get/Set Key Beep | Prg | `KBP` | `KBP,[LEVEL],[LOCK]` | `LEVEL`: 0(Auto), 99(Off). `LOCK`: 0(Off), 1(On). |
| **PRI** | Get/Set Priority Mode | Prg | `PRI` | `PRI,[PRI_MODE]` | `PRI_MODE`: 0(Off), 1(On), 2(Plus On), 3(DND). |
| **SCG** | Get/Set Scan Channel Group | Prg | `SCG` | `SCG,##########` | `##########`: 10 digits (1-10). 0=Valid, 1=Invalid (Lockout). |
| **DCH** | Delete Channel | Prg | - | `DCH,[INDEX]` | `INDEX`: 1-500. |
| **CIN** | Get/Set Channel Info | Prg | `CIN,[INDEX]` | `CIN,[INDEX],[NAME],[FRQ],[MOD],[CTCSS/DCS],[DLY],[LOUT],[PRI]` | `INDEX`: 1-500. `MOD`: Auto/AM/FM/NFM. |
| **SCO** | Get/Set Search/Close Call Settings | Prg | `SCO` | `SCO,[DLY],[CODE_SRCH]` | `DLY`: -10,-5,0,1,2,3,4,5. `CODE_SRCH`: 0(Off), 1(On). |
| **GLF** | Get Global Lockout Freq | Prg | `GLF` | `GLF,[***]` | Retrieve list until returns `-1`. `***`: Don't care. |
| **ULF** | Unlock Global L/O | Prg | - | `ULF,[FRQ]` | Unlocks a frequency from Global L/O list. |
| **LOF** | Lock Out Frequency | Prg | - | `LOF,[FRQ]` | Locks out a frequency (adds to L/O list). |
| **CLC** | Get/Set Close Call Settings | Prg | `CLC` | `CLC,[CC_MODE],[ALTB],[ALTL],[CC_BAND],[LOUT]` | `CC_MODE`: 0(Off), 1(Pri), 2(DND). `CC_BAND`: 5 digits mask. |
| **SSG** | Get/Set Service Search Group | Prg | `SSG` | `SSG,##########` | `##########`: 10 digits mask (Racing, FRS, CB, etc). 0=Valid, 1=Invalid. |
| **CSG** | Get/Set Custom Search Group | Prg | `CSG` | `CSG,##########` | `##########`: 10 digits mask (Ranges 1-10). 0=Valid, 1=Invalid. |
| **CSP** | Get/Set Custom Search Settings | Prg | `CSP,[INDEX]` | `CSP,[INDEX],[LIMIT_L],[LIMIT_H]` | `INDEX`: 1-10. Limits in Hz (e.g. 250000). |
| **WXS** | Get/Set Weather Settings | Prg | `WXS` | `WXS,[ALT_PRI]` | `ALT_PRI`: 0(Off), 1(On). |
| **CNT** | Get/Set LCD Contrast | Prg | `CNT` | `CNT,[CONTRAST]` | `CONTRAST`: 1-15. |
| **VOL** | Get/Set Volume Level | All | `VOL` | `VOL,[LEVEL]` | `LEVEL`: 0-15. |
| **SQL** | Get/Set Squelch Level | All | `SQL` | `SQL,[LEVEL]` | `LEVEL`: 0(Open) - 14 - 15(Close). |

## Reverse Engineered Commands
| Command | Description | Mode | Controller Format (Get/Action) | Controller Format (Set) | Notes |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **GLG** | Current Scanning Status | All | `GLG` | - | GLG,[Freq],[Modulation],,[0],,,[Channel Name],0,1,,[Channel Index], Example : GLG,01239750,AM,,0,,,BHX RADAR,1,0,,52, |
| **STS** | Another Status Command | All | `STS` | - ||
| **KEY** | Send KeyPress | All | `KEY` | `KEY,[K1],[K2]` | Sends KeyPresses as if scanner physical buttons had been pressed |


## Miscellaneous Command Examples

### Scan bank 2

PRG
SCG,1011111111
EPG

## Start Scan

Note: this would be required after using `SCG` to change selection of scan channel banks, for example.

KEY,S,P

## Hold Scan

Note: this can be repeated to toggle hold.

KEY,H,P
