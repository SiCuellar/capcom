#!/usr/bin/env bash
# One-time: create a self-signed code-signing identity "Capcom Local Signing" in the login keychain.
# Needed so rebuilt Capcom.app keeps the same code identity (TCC permission grants persist).
# macOS will ask for your login password / approval when trusting the certificate.
set -euo pipefail

NAME="${CAPCOM_SIGN_IDENTITY:-Capcom Local Signing}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning | grep -q "$NAME"; then
  echo "identity '$NAME' already exists; nothing to do."
  exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
PASS="capcom-$$-$RANDOM"

cat > "$TMP/openssl.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout "$TMP/key.pem" -out "$TMP/cert.pem" -config "$TMP/openssl.cnf"
/usr/bin/openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" \
  -out "$TMP/id.p12" -passout "pass:$PASS" -name "$NAME"

security import "$TMP/id.p12" -k "$KEYCHAIN" -P "$PASS" -T /usr/bin/codesign
# Trust it for code signing (prompts for approval).
security add-trusted-cert -d -r trustRoot -p codeSign -k "$KEYCHAIN" "$TMP/cert.pem"

echo "created identity:"
security find-identity -v -p codesigning | grep "$NAME" || echo "(not listed as valid yet; check Keychain Access trust settings)"
