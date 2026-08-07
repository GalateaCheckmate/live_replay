from pathlib import Path
import xml.etree.ElementTree as ET

ANDROID_NS = "http://schemas.android.com/apk/res/android"
ET.register_namespace("android", ANDROID_NS)
name_attr = f"{{{ANDROID_NS}}}name"

manifest_path = Path("tauri-app/src-tauri/gen/android/app/src/main/AndroidManifest.xml")
if not manifest_path.exists():
    raise SystemExit(f"AndroidManifest.xml not found: {manifest_path}")

tree = ET.parse(manifest_path)
root = tree.getroot()

required_permissions = [
    "android.permission.INTERNET",
    "android.permission.ACCESS_NETWORK_STATE",
    "android.permission.WAKE_LOCK",
    "android.permission.FOREGROUND_SERVICE",
    "android.permission.FOREGROUND_SERVICE_DATA_SYNC",
    "android.permission.POST_NOTIFICATIONS",
]

existing = {
    item.get(name_attr)
    for item in root.findall("uses-permission")
    if item.get(name_attr)
}

for permission in required_permissions:
    if permission not in existing:
        node = ET.Element("uses-permission")
        node.set(name_attr, permission)
        root.insert(0, node)

application = root.find("application")
if application is None:
    raise SystemExit("Android manifest has no <application> element")

# Keep the recorder process simple and predictable on the P40. The native
# recorder will own long-running work; the WebView is only the control UI.
application.set(f"{{{ANDROID_NS}}}hardwareAccelerated", "true")
application.set(f"{{{ANDROID_NS}}}usesCleartextTraffic", "true")

ET.indent(tree, space="    ")
tree.write(manifest_path, encoding="utf-8", xml_declaration=True)

print("Huawei P40 manifest tuning applied:")
for permission in required_permissions:
    print(f"  - {permission}")
