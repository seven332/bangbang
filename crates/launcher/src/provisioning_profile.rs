use std::{fmt, time::SystemTime};

use plist::{Data, Date};
use serde::Deserialize;

use crate::layout::{
    APP_SANDBOX_ENTITLEMENT, APPLICATION_IDENTIFIER_ENTITLEMENT, HYPERVISOR_ENTITLEMENT,
    TEAM_IDENTIFIER_ENTITLEMENT, VMNET_ENTITLEMENT,
};
use crate::{PackageError, WORKER_BUNDLE_IDENTIFIER};

pub(crate) const MAX_PROVISIONING_PROFILE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PROFILE_CERTIFICATES: usize = 16;
const MAX_PROFILE_CERTIFICATE_BYTES: usize = 64 * 1024;
const TEAM_COMPONENT_BYTES: usize = 10;
#[cfg(test)]
const DEVELOPER_VMNET_ENTITLEMENT: &str = "com.apple.developer.networking.vmnet";

#[derive(Deserialize)]
struct DecodedProvisioningProfile {
    #[serde(rename = "TeamIdentifier")]
    team_identifiers: Vec<String>,
    #[serde(rename = "ApplicationIdentifierPrefix")]
    application_identifier_prefixes: Vec<String>,
    #[serde(rename = "CreationDate")]
    creation_date: Date,
    #[serde(rename = "ExpirationDate")]
    expiration_date: Date,
    #[serde(rename = "DeveloperCertificates")]
    developer_certificates: Vec<Data>,
    #[serde(rename = "Entitlements")]
    entitlements: DecodedEntitlements,
}

#[derive(Deserialize)]
struct DecodedEntitlements {
    #[serde(rename = "com.apple.application-identifier")]
    application_identifier: String,
    #[serde(rename = "com.apple.developer.team-identifier")]
    team_identifier: String,
    #[serde(rename = "com.apple.vm.networking")]
    vmnet: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ApprovedProvisioningProfile {
    application_identifier: String,
    team_identifier: String,
    developer_certificates: Vec<Vec<u8>>,
}

impl fmt::Debug for ApprovedProvisioningProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApprovedProvisioningProfile(<redacted>)")
    }
}

impl ApprovedProvisioningProfile {
    pub(crate) fn parse(bytes: &[u8], now: SystemTime) -> Result<Self, PackageError> {
        if bytes.is_empty() || bytes.len() > MAX_PROVISIONING_PROFILE_BYTES {
            return Err(PackageError::InvalidProvisioningProfile);
        }
        // Strongly typed deserialization rejects duplicate critical keys instead
        // of accepting the last value. Unknown Apple profile metadata remains
        // intentionally outside this narrow authorization contract.
        let decoded: DecodedProvisioningProfile =
            plist::from_bytes(bytes).map_err(|_| PackageError::InvalidProvisioningProfile)?;

        let team_identifier = one_identifier(decoded.team_identifiers)?;
        let application_prefix = one_identifier(decoded.application_identifier_prefixes)?;
        let expected_application_identifier =
            format!("{application_prefix}.{WORKER_BUNDLE_IDENTIFIER}");

        let creation: SystemTime = decoded.creation_date.into();
        let expiration: SystemTime = decoded.expiration_date.into();
        if creation > now || expiration <= now || creation >= expiration {
            return Err(PackageError::InvalidProvisioningProfile);
        }

        if decoded.entitlements.application_identifier != expected_application_identifier
            || decoded.entitlements.team_identifier != team_identifier
            || !decoded.entitlements.vmnet
        {
            return Err(PackageError::InvalidProvisioningProfile);
        }
        // The developer-prefixed key is deliberately not consulted. If it is
        // present alongside the documented runtime authorization it remains
        // profile metadata; it is never copied into the code signature.
        let certificates = decoded.developer_certificates;
        if certificates.is_empty() || certificates.len() > MAX_PROFILE_CERTIFICATES {
            return Err(PackageError::InvalidProvisioningProfile);
        }
        let mut total = 0_usize;
        let mut developer_certificates = Vec::with_capacity(certificates.len());
        for certificate in certificates {
            let certificate = certificate.as_ref();
            if certificate.is_empty() || certificate.len() > MAX_PROFILE_CERTIFICATE_BYTES {
                return Err(PackageError::InvalidProvisioningProfile);
            }
            total = total
                .checked_add(certificate.len())
                .filter(|total| *total <= MAX_PROVISIONING_PROFILE_BYTES)
                .ok_or(PackageError::InvalidProvisioningProfile)?;
            developer_certificates.push(certificate.to_vec());
        }

        Ok(Self {
            application_identifier: expected_application_identifier,
            team_identifier,
            developer_certificates,
        })
    }

    pub(crate) fn application_identifier(&self) -> &str {
        &self.application_identifier
    }

    pub(crate) fn team_identifier(&self) -> &str {
        &self.team_identifier
    }

    pub(crate) fn permits_certificate(&self, certificate: &[u8]) -> bool {
        self.developer_certificates
            .iter()
            .any(|candidate| candidate == certificate)
    }

    pub(crate) fn entitlement_plist(&self) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>{APP_SANDBOX_ENTITLEMENT}</key>
  <true/>
  <key>{HYPERVISOR_ENTITLEMENT}</key>
  <true/>
  <key>{VMNET_ENTITLEMENT}</key>
  <true/>
  <key>{APPLICATION_IDENTIFIER_ENTITLEMENT}</key>
  <string>{}</string>
  <key>{TEAM_IDENTIFIER_ENTITLEMENT}</key>
  <string>{}</string>
</dict>
</plist>
"#,
            self.application_identifier, self.team_identifier
        )
        .into_bytes()
    }
}

pub(crate) fn valid_team_identifier(value: &str) -> bool {
    value.len() == TEAM_COMPONENT_BYTES && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

pub(crate) fn valid_application_identifier(value: &str) -> bool {
    let Some(prefix) = value.strip_suffix(&format!(".{WORKER_BUNDLE_IDENTIFIER}")) else {
        return false;
    };
    valid_team_identifier(prefix)
}

fn one_identifier(values: Vec<String>) -> Result<String, PackageError> {
    if values.len() != 1 {
        return Err(PackageError::InvalidProvisioningProfile);
    }
    let value = values
        .into_iter()
        .next()
        .filter(|value| valid_team_identifier(value))
        .ok_or(PackageError::InvalidProvisioningProfile)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use plist::Value;

    use super::*;

    const VALID_PROFILE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>TeamIdentifier</key><array><string>TEAM123456</string></array>
<key>ApplicationIdentifierPrefix</key><array><string>APPID12345</string></array>
<key>CreationDate</key><date>2026-01-01T00:00:00Z</date>
<key>ExpirationDate</key><date>2030-01-01T00:00:00Z</date>
<key>DeveloperCertificates</key><array><data>AQID</data></array>
<key>Entitlements</key><dict>
<key>com.apple.application-identifier</key><string>APPID12345.dev.bangbang.worker</string>
<key>com.apple.developer.team-identifier</key><string>TEAM123456</string>
<key>com.apple.vm.networking</key><true/>
</dict>
</dict></plist>"#;

    fn now() -> SystemTime {
        plist::Date::from_xml_format("2027-01-01T00:00:00Z")
            .expect("test date should parse")
            .into()
    }

    #[test]
    fn parses_exact_relationships_and_generates_five_claims() {
        let profile = ApprovedProvisioningProfile::parse(VALID_PROFILE.as_bytes(), now())
            .expect("profile should parse");
        assert_eq!(
            profile.application_identifier(),
            "APPID12345.dev.bangbang.worker"
        );
        assert_eq!(profile.team_identifier(), "TEAM123456");
        assert!(profile.permits_certificate(&[1, 2, 3]));
        assert!(!profile.permits_certificate(&[3, 2, 1]));
        let entitlements = String::from_utf8(profile.entitlement_plist())
            .expect("generated entitlements should be UTF-8");
        assert_eq!(entitlements.matches("<key>").count(), 5);
        assert!(entitlements.contains("<string>APPID12345.dev.bangbang.worker</string>"));
        assert!(entitlements.contains("<string>TEAM123456</string>"));
        assert!(!entitlements.contains(DEVELOPER_VMNET_ENTITLEMENT));
        assert_eq!(
            format!("{profile:?}"),
            "ApprovedProvisioningProfile(<redacted>)"
        );
    }

    #[test]
    fn accepts_separate_app_prefix_and_unrelated_developer_metadata() {
        let profile = VALID_PROFILE.replace(
            "<key>com.apple.vm.networking</key><true/>",
            "<key>com.apple.vm.networking</key><true/><key>com.apple.developer.networking.vmnet</key><true/>",
        );
        ApprovedProvisioningProfile::parse(profile.as_bytes(), now())
            .expect("documented authorization should remain independently sufficient");
    }

    #[test]
    fn rejects_developer_only_false_or_mismatched_authorization() {
        for invalid in [
            VALID_PROFILE.replace("com.apple.vm.networking", DEVELOPER_VMNET_ENTITLEMENT),
            VALID_PROFILE.replace(
                "<key>com.apple.vm.networking</key><true/>",
                "<key>com.apple.vm.networking</key><false/>",
            ),
            VALID_PROFILE.replace(
                "<key>com.apple.vm.networking</key><true/>",
                "<key>com.apple.vm.networking</key><string>true</string>",
            ),
            VALID_PROFILE.replace("APPID12345.dev.bangbang.worker", "APPID12345.dev.other"),
            VALID_PROFILE.replace(
                "<string>TEAM123456</string>\n<key>com.apple.vm.networking",
                "<string>OTHER12345</string>\n<key>com.apple.vm.networking",
            ),
        ] {
            assert_eq!(
                ApprovedProvisioningProfile::parse(invalid.as_bytes(), now()),
                Err(PackageError::InvalidProvisioningProfile)
            );
        }
    }

    #[test]
    fn rejects_invalid_identifier_arrays_and_grammar() {
        for invalid in [
            VALID_PROFILE.replace("TeamIdentifier", "MissingTeamIdentifier"),
            VALID_PROFILE.replace(
                "<array><string>TEAM123456</string></array>",
                "<string>TEAM123456</string>",
            ),
            VALID_PROFILE.replace(
                "<array><string>TEAM123456</string></array>",
                "<array></array>",
            ),
            VALID_PROFILE.replace(
                "<array><string>TEAM123456</string></array>",
                "<array><string>TEAM123456</string><string>OTHER12345</string></array>",
            ),
            VALID_PROFILE.replace("TEAM123456", "TEAM-12345"),
            VALID_PROFILE.replace("APPID12345", "SHORT"),
        ] {
            assert_eq!(
                ApprovedProvisioningProfile::parse(invalid.as_bytes(), now()),
                Err(PackageError::InvalidProvisioningProfile)
            );
        }
    }

    #[test]
    fn rejects_duplicate_critical_profile_and_entitlement_keys() {
        for invalid in [
            VALID_PROFILE.replace(
                "<key>TeamIdentifier</key><array><string>TEAM123456</string></array>",
                "<key>TeamIdentifier</key><array><string>TEAM123456</string></array><key>TeamIdentifier</key><array><string>TEAM123456</string></array>",
            ),
            VALID_PROFILE.replace(
                "<key>com.apple.application-identifier</key><string>APPID12345.dev.bangbang.worker</string>",
                "<key>com.apple.application-identifier</key><string>APPID12345.dev.bangbang.worker</string><key>com.apple.application-identifier</key><string>APPID12345.dev.bangbang.worker</string>",
            ),
            VALID_PROFILE.replace(
                "<key>com.apple.vm.networking</key><true/>",
                "<key>com.apple.vm.networking</key><true/><key>com.apple.vm.networking</key><true/>",
            ),
        ] {
            assert_eq!(
                ApprovedProvisioningProfile::parse(invalid.as_bytes(), now()),
                Err(PackageError::InvalidProvisioningProfile)
            );
        }
    }

    #[test]
    fn rejects_invalid_validity_windows() {
        for invalid in [
            VALID_PROFILE.replace("2026-01-01T00:00:00Z", "2028-01-01T00:00:00Z"),
            VALID_PROFILE.replace("2030-01-01T00:00:00Z", "2027-01-01T00:00:00Z"),
            VALID_PROFILE.replace("2030-01-01T00:00:00Z", "2025-01-01T00:00:00Z"),
        ] {
            assert_eq!(
                ApprovedProvisioningProfile::parse(invalid.as_bytes(), now()),
                Err(PackageError::InvalidProvisioningProfile)
            );
        }
    }

    #[test]
    fn rejects_malformed_empty_oversized_or_unbounded_certificate_inputs() {
        assert_eq!(
            ApprovedProvisioningProfile::parse(b"not a plist", now()),
            Err(PackageError::InvalidProvisioningProfile)
        );
        assert_eq!(
            ApprovedProvisioningProfile::parse(&[], now()),
            Err(PackageError::InvalidProvisioningProfile)
        );
        assert_eq!(
            ApprovedProvisioningProfile::parse(
                &vec![0_u8; MAX_PROVISIONING_PROFILE_BYTES + 1],
                now()
            ),
            Err(PackageError::InvalidProvisioningProfile)
        );
        let no_certificates = VALID_PROFILE.replace("<array><data>AQID</data></array>", "<array/>");
        assert_eq!(
            ApprovedProvisioningProfile::parse(no_certificates.as_bytes(), now()),
            Err(PackageError::InvalidProvisioningProfile)
        );
        let too_many = VALID_PROFILE.replace(
            "<array><data>AQID</data></array>",
            &format!("<array>{}</array>", "<data>AQID</data>".repeat(17)),
        );
        assert_eq!(
            ApprovedProvisioningProfile::parse(too_many.as_bytes(), now()),
            Err(PackageError::InvalidProvisioningProfile)
        );

        let mut oversized_certificate = Value::from_reader(Cursor::new(VALID_PROFILE.as_bytes()))
            .expect("fixture should parse");
        oversized_certificate
            .as_dictionary_mut()
            .expect("fixture root should be a dictionary")
            .insert(
                "DeveloperCertificates".to_owned(),
                Value::Array(vec![Value::Data(vec![
                    0_u8;
                    MAX_PROFILE_CERTIFICATE_BYTES + 1
                ])]),
            );
        let mut bytes = Vec::new();
        oversized_certificate
            .to_writer_xml(&mut bytes)
            .expect("oversized certificate fixture should encode");
        assert_eq!(
            ApprovedProvisioningProfile::parse(&bytes, now()),
            Err(PackageError::InvalidProvisioningProfile)
        );
    }

    #[test]
    fn runtime_identifier_shapes_are_closed() {
        assert!(valid_team_identifier("TEAM123456"));
        assert!(!valid_team_identifier("TEAM-12345"));
        assert!(valid_application_identifier(
            "APPID12345.dev.bangbang.worker"
        ));
        assert!(!valid_application_identifier("APPID12345.dev.other"));
    }
}
