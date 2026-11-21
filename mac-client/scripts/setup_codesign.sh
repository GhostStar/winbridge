#!/usr/bin/env bash
set -euo pipefail

# Creates a persistent 10-year self-signed Code Signing certificate ("WinBridge Local Dev")
# in your macOS login Keychain so rebuilding WinBridge.app preserves its designated code identity
# and avoids repeated macOS Local Network / Keychain permission prompts.

CERT_NAME="${1:-WinBridge Local Dev}"

if security find-identity -v -p codesigning 2>/dev/null | grep -Fq "\"$CERT_NAME\""; then
    echo "Code-signing identity \"$CERT_NAME\" already exists in your Keychain."
    echo "You can now run ./scripts/bundle_app.sh directly."
    exit 0
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

cat > "$TMP_DIR/cert.cnf" << EOF
[ req ]
default_bits       = 2048
distinguished_name = req_dn
x509_extensions    = codesign_ext
prompt             = no

[ req_dn ]
CN = $CERT_NAME
O  = WinBridge Local

[ codesign_ext ]
basicConstraints = critical, CA:FALSE
keyUsage         = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF

openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$TMP_DIR/key.pem" \
    -out "$TMP_DIR/cert.pem" \
    -days 3650 \
    -config "$TMP_DIR/cert.cnf" >/dev/null 2>&1

P12_PASS="winbridge-local-dev"
openssl pkcs12 -export \
    -out "$TMP_DIR/identity.p12" \
    -inkey "$TMP_DIR/key.pem" \
    -in "$TMP_DIR/cert.pem" \
    -passout "pass:$P12_PASS" \
    -legacy >/dev/null 2>&1 || \
openssl pkcs12 -export \
    -out "$TMP_DIR/identity.p12" \
    -inkey "$TMP_DIR/key.pem" \
    -in "$TMP_DIR/cert.pem" \
    -passout "pass:$P12_PASS" >/dev/null 2>&1

LOGIN_KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
security import "$TMP_DIR/identity.p12" \
    -k "$LOGIN_KEYCHAIN" \
    -P "$P12_PASS" \
    -T /usr/bin/codesign \
    -T /usr/bin/security

echo ""
echo "Imported self-signed code-signing identity \"$CERT_NAME\" into login Keychain."
echo "Final one-time step: mark it trusted for Code Signing so codesign accepts it:"
echo "  security add-trusted-cert -p codeSign -k \"$LOGIN_KEYCHAIN\" <(security find-certificate -c \"$CERT_NAME\" -p)"
echo "(Or open Keychain Access -> login -> \"$CERT_NAME\" -> Trust -> Code Signing: Always Trust)"
echo ""
echo "Once trusted, ./scripts/bundle_app.sh will automatically sign WinBridge.app with \"$CERT_NAME\"."
