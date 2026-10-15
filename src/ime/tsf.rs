//! TSF input processor profile activate / enumerate.
//! Modern Chinese IMEs (WeChat / Microsoft Pinyin) are TIP profiles, not distinct HKLs.

use windows::core::GUID;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;

use windows::Win32::UI::TextServices::{
    CLSID_TF_InputProcessorProfiles, CLSID_TF_ThreadMgr, ITfCompartment, ITfCompartmentMgr,
    ITfInputProcessorProfileMgr, ITfThreadMgr, GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION,
    GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, GUID_TFCAT_TIP_KEYBOARD, TF_CONVERSIONMODE_NATIVE,
    TF_INPUTPROCESSORPROFILE, TF_IPPMF_ENABLEPROFILE, TF_IPPMF_FORSESSION,
    TF_PROFILETYPE_INPUTPROCESSOR,
};

const LANG_ZH_CN: u16 = 0x0804;

/// Microsoft Pinyin text service CLSID.
pub const CLSID_MS_PINYIN: GUID = GUID::from_u128(0x81D4E9C9_1D3B_41BC_9E6C_4B40BF79E35E);
/// WeChat IME text service CLSID (from this machine's InputMethodTips).
pub const CLSID_WECHAT_IME: GUID = GUID::from_u128(0x86598FB9_66A2_463E_B9C2_AEB906D477AD);

static COM_INIT: std::sync::Once = std::sync::Once::new();

pub fn ensure_com() {
    COM_INIT.call_once(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    });
}

fn profile_mgr() -> windows::core::Result<ITfInputProcessorProfileMgr> {
    ensure_com();
    unsafe { CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER) }
}

#[derive(Debug, Clone)]
pub struct TipProfile {
    pub langid: u16,
    pub clsid: GUID,
    pub guid_profile: GUID,
    pub backend: crate::ime::ActiveIme,
}

pub fn enum_zh_profiles() -> Vec<TipProfile> {
    let Ok(mgr) = profile_mgr() else {
        return vec![];
    };
    let Ok(enumerator) = (unsafe { mgr.EnumProfiles(LANG_ZH_CN) }) else {
        return vec![];
    };

    let mut out = Vec::new();
    loop {
        let mut buf = [TF_INPUTPROCESSORPROFILE::default(); 1];
        let mut fetched = 0u32;
        if unsafe { enumerator.Next(&mut buf, &mut fetched) }.is_err() || fetched == 0 {
            break;
        }
        let p = buf[0];
        if p.dwProfileType != TF_PROFILETYPE_INPUTPROCESSOR {
            continue;
        }
        // Skip noise profiles without a real langid.
        if p.langid != LANG_ZH_CN {
            continue;
        }
        let backend = classify_clsid(&p.clsid);
        out.push(TipProfile {
            langid: p.langid,
            clsid: p.clsid,
            guid_profile: p.guidProfile,
            backend,
        });
    }
    out
}

pub fn classify_clsid(clsid: &GUID) -> crate::ime::ActiveIme {
    if *clsid == CLSID_WECHAT_IME {
        crate::ime::ActiveIme::WeChatIme
    } else if *clsid == CLSID_MS_PINYIN {
        crate::ime::ActiveIme::MsPinyin
    } else {
        crate::ime::ActiveIme::OtherChinese
    }
}

pub fn active_keyboard_profile() -> Option<TipProfile> {
    let mgr = profile_mgr().ok()?;
    let mut profile = TF_INPUTPROCESSORPROFILE::default();
    unsafe {
        mgr.GetActiveProfile(&GUID_TFCAT_TIP_KEYBOARD, &mut profile)
            .ok()?;
    }
    if profile.dwProfileType == 0 {
        return None;
    }
    // Keyboard layout (not TIP) → treat as ENG-ish if lang is en.
    if profile.dwProfileType != TF_PROFILETYPE_INPUTPROCESSOR {
        let lang = profile.langid;
        let backend = if lang == 0x0409 {
            crate::ime::ActiveIme::LayoutEnglish
        } else {
            crate::ime::ActiveIme::Unknown
        };
        return Some(TipProfile {
            langid: lang,
            clsid: profile.clsid,
            guid_profile: profile.guidProfile,
            backend,
        });
    }
    let backend = classify_clsid(&profile.clsid);
    Some(TipProfile {
        langid: profile.langid,
        clsid: profile.clsid,
        guid_profile: profile.guidProfile,
        backend,
    })
}

pub fn activate_tip(clsid: &GUID, guid_profile: &GUID, langid: u16) -> Result<(), String> {
    let mgr = profile_mgr().map_err(|e| format!("CoCreate profile mgr: {e}"))?;
    unsafe {
        mgr.ActivateProfile(
            TF_PROFILETYPE_INPUTPROCESSOR,
            langid,
            clsid,
            guid_profile,
            HKL::default(),
            TF_IPPMF_FORSESSION | TF_IPPMF_ENABLEPROFILE,
        )
        .map_err(|e| format!("ActivateProfile: {e}"))
    }
}

/// Prefer Microsoft Pinyin; optional legacy WeChat-first path for experiments only.
pub fn activate_preferred_chinese(prefer_wechat: bool) -> Result<TipProfile, String> {
    let profiles = enum_zh_profiles();
    if profiles.is_empty() {
        return Err("no zh-CN TIP profiles found".into());
    }

    let pick = if prefer_wechat {
        profiles
            .iter()
            .find(|p| p.backend == crate::ime::ActiveIme::WeChatIme)
            .or_else(|| {
                profiles
                    .iter()
                    .find(|p| p.backend == crate::ime::ActiveIme::MsPinyin)
            })
            .or_else(|| profiles.first())
    } else {
        profiles
            .iter()
            .find(|p| p.backend == crate::ime::ActiveIme::MsPinyin)
            .or_else(|| {
                profiles
                    .iter()
                    .find(|p| p.backend == crate::ime::ActiveIme::OtherChinese)
            })
            .or_else(|| profiles.first())
    }
    .cloned()
    .ok_or_else(|| "no tip to activate".to_string())?;

    activate_tip(&pick.clsid, &pick.guid_profile, pick.langid)?;
    Ok(pick)
}

/// Activate a keyboard-layout style English profile if present in language bar.
pub fn activate_english_via_tsf() -> Result<(), String> {
    let mgr = profile_mgr().map_err(|e| format!("CoCreate profile mgr: {e}"))?;
    // Enum all profiles for en-US (0x0409)
    let enumerator =
        unsafe { mgr.EnumProfiles(0x0409) }.map_err(|e| format!("EnumProfiles en-US: {e}"))?;

    {
        let mut buf = [TF_INPUTPROCESSORPROFILE::default(); 1];
        let mut fetched = 0u32;
        if unsafe { enumerator.Next(&mut buf, &mut fetched) }.is_err() || fetched == 0 {
            return Err("no en-US input profile installed".into());
        }
        let p = buf[0];
        unsafe {
            mgr.ActivateProfile(
                p.dwProfileType,
                p.langid,
                &p.clsid,
                &p.guidProfile,
                p.hkl,
                TF_IPPMF_FORSESSION | TF_IPPMF_ENABLEPROFILE,
            )
            .map_err(|e| format!("ActivateProfile en: {e}"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompartmentSnapshot {
    pub open: Option<bool>,
    pub conversion_raw: Option<u32>,
    pub native: Option<bool>,
    pub error: Option<String>,
}

impl CompartmentSnapshot {
    pub fn describe(&self) -> String {
        match (self.open, self.native, self.conversion_raw) {
            (Some(open), Some(native), Some(raw)) => {
                format!("open={open} native={native} raw=0x{raw:x}")
            }
            _ => self.error.clone().unwrap_or_else(|| "unavailable".into()),
        }
    }
}

/// Session/global diagnostic signal; never evidence of the target application's input context.
pub fn read_global_compartments() -> CompartmentSnapshot {
    ensure_com();
    unsafe {
        let mgr: ITfThreadMgr =
            match CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER) {
                Ok(m) => m,
                Err(e) => {
                    return CompartmentSnapshot {
                        error: Some(format!("CoCreate ThreadMgr: {e}")),
                        ..Default::default()
                    };
                }
            };
        let _ = mgr.Activate();
        let compartment_mgr = match mgr.GetGlobalCompartment() {
            Ok(c) => c,
            Err(e) => {
                let _ = mgr.Deactivate();
                return CompartmentSnapshot {
                    error: Some(format!("GetGlobalCompartment: {e}")),
                    ..Default::default()
                };
            }
        };
        let open = compartment_bool(&compartment_mgr, &GUID_COMPARTMENT_KEYBOARD_OPENCLOSE);
        let conversion_raw = compartment_u32(
            &compartment_mgr,
            &GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION,
        );
        let native = conversion_raw.map(|v| (v & TF_CONVERSIONMODE_NATIVE) != 0);
        let _ = mgr.Deactivate();
        CompartmentSnapshot {
            open,
            conversion_raw,
            native,
            error: None,
        }
    }
}

fn compartment_u32(mgr: &ITfCompartmentMgr, guid: &GUID) -> Option<u32> {
    unsafe {
        let comp: ITfCompartment = mgr.GetCompartment(guid).ok()?;
        variant_u32(&comp.GetValue().ok()?)
    }
}

fn compartment_bool(mgr: &ITfCompartmentMgr, guid: &GUID) -> Option<bool> {
    compartment_u32(mgr, guid).map(|v| v != 0)
}

fn variant_u32(value: &windows::core::VARIANT) -> Option<u32> {
    u32::try_from(value)
        .ok()
        .or_else(|| i32::try_from(value).ok().map(|v| v as u32))
}

pub fn format_clsid(clsid: &GUID) -> String {
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        clsid.data1,
        clsid.data2,
        clsid.data3,
        clsid.data4[0],
        clsid.data4[1],
        clsid.data4[2],
        clsid.data4[3],
        clsid.data4[4],
        clsid.data4[5],
        clsid.data4[6],
        clsid.data4[7],
    )
}
