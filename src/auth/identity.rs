use serde::Deserialize;

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct TokenIdentity {
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
    pub personal_org_id: Option<String>,
    pub orgs: Vec<TokenOrg>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TokenOrg {
    pub org_id: Option<String>,
    pub org_name: Option<String>,
    pub org_slug: Option<String>,
    pub role: Option<String>,
}

#[derive(Default, Debug, Clone, Deserialize)]
struct AccessTokenClaims {
    pub sub: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub orgs: Vec<TokenOrg>,
    pub personal_org_id: Option<String>,
}

pub fn extract_identity_from_access_token(access_token: &str) -> TokenIdentity {
    let mut parts = access_token.split('.');
    let _header = parts.next();
    let payload = match parts.next() {
        Some(payload) if !payload.is_empty() => payload,
        _ => return TokenIdentity::default(),
    };
    let _signature = parts.next();

    let decoded = match decode_base64_url(payload) {
        Ok(decoded) => decoded,
        Err(_) => return TokenIdentity::default(),
    };

    let claims: AccessTokenClaims = match serde_json::from_slice(&decoded) {
        Ok(claims) => claims,
        Err(_) => return TokenIdentity::default(),
    };

    TokenIdentity {
        user_id: claims.sub,
        email: claims.email,
        name: claims.name,
        personal_org_id: claims.personal_org_id,
        orgs: claims.orgs,
    }
}

fn decode_base64_url(input: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = 0u32;
    let mut bit_count = 0u8;

    for ch in input.chars() {
        if ch == '=' {
            break;
        }

        let value = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => 26 + (ch as u32 - 'a' as u32),
            '0'..='9' => 52 + (ch as u32 - '0' as u32),
            '-' => 62,
            '_' => 63,
            _ => return Err(format!("invalid base64url character '{}'", ch)),
        };

        buffer = (buffer << 6) | value;
        bit_count += 6;

        while bit_count >= 8 {
            bit_count -= 8;
            bytes.push(((buffer >> bit_count) & 0xFF) as u8);
        }
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_identity_from_access_token() {
        let token = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1MTIzIiwiZW1haWwiOiJ1c2VyQGV4YW1wbGUuY29tIiwibmFtZSI6Ik5hbWUiLCJvcmdzIjpbeyJvcmdfaWQiOiJvcmcxIiwib3JnX25hbWUiOiJPcmcgT25lIiwib3JnX3NsdWciOiJvcmctb25lIiwicm9sZSI6Im93bmVyIn1dLCJwZXJzb25hbF9vcmdfaWQiOiJvcmcxIn0.";
        let identity = extract_identity_from_access_token(token);
        assert_eq!(identity.user_id.as_deref(), Some("u123"));
        assert_eq!(identity.email.as_deref(), Some("user@example.com"));
        assert_eq!(identity.name.as_deref(), Some("Name"));
        assert_eq!(identity.personal_org_id.as_deref(), Some("org1"));
        assert_eq!(identity.orgs.len(), 1);
        assert_eq!(identity.orgs[0].org_slug.as_deref(), Some("org-one"));
    }

    #[test]
    fn test_extract_identity_handles_non_jwt_token() {
        let identity = extract_identity_from_access_token("opaque-token");
        assert!(identity.user_id.is_none());
        assert!(identity.email.is_none());
        assert!(identity.name.is_none());
        assert!(identity.personal_org_id.is_none());
        assert!(identity.orgs.is_empty());
    }
}
