from pathlib import Path
import re
import xml.etree.ElementTree as ET

ANDROID_NS = "http://schemas.android.com/apk/res/android"
ET.register_namespace("android", ANDROID_NS)
name_attr = f"{{{ANDROID_NS}}}name"
android_attr = lambda name: f"{{{ANDROID_NS}}}{name}"

android_root = Path("tauri-app/src-tauri/gen/android/app/src/main")
manifest_path = android_root / "AndroidManifest.xml"
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

# Live streams may still resolve to plain HTTP FLV/HLS endpoints. Do not let
# Android's cleartext policy break a source that the desktop recorder accepts.
application.set(android_attr("hardwareAccelerated"), "true")
application.set(android_attr("usesCleartextTraffic"), "true")

service_name = ".LiveReplayForegroundService"
service = next(
    (item for item in application.findall("service") if item.get(name_attr) == service_name),
    None,
)
if service is None:
    service = ET.SubElement(application, "service")
service.set(name_attr, service_name)
service.set(android_attr("enabled"), "true")
service.set(android_attr("exported"), "false")
service.set(android_attr("foregroundServiceType"), "dataSync")
service.set(android_attr("stopWithTask"), "false")

ET.indent(tree, space="    ")
tree.write(manifest_path, encoding="utf-8", xml_declaration=True)

java_root = android_root / "java"
main_activity_files = list(java_root.rglob("MainActivity.kt"))
if len(main_activity_files) != 1:
    raise SystemExit(f"Expected exactly one MainActivity.kt, found {len(main_activity_files)}")

main_activity_path = main_activity_files[0]
activity_text = main_activity_path.read_text(encoding="utf-8")
package_match = re.search(r"^package\s+([\w.]+)\s*$", activity_text, re.MULTILINE)
if not package_match:
    raise SystemExit("Could not determine Android package from MainActivity.kt")
package_name = package_match.group(1)

for import_line in [
    "import android.content.Intent",
    "import android.os.Build",
    "import android.os.Bundle",
]:
    if import_line not in activity_text:
        package_line = f"package {package_name}"
        activity_text = activity_text.replace(package_line, package_line + "\n\n" + import_line, 1)

simple_class = "class MainActivity : TauriActivity()"
if simple_class in activity_text:
    activity_text = activity_text.replace(
        simple_class,
        """class MainActivity : TauriActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val serviceIntent = Intent(this, LiveReplayForegroundService::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            startForegroundService(serviceIntent)
        } else {
            startService(serviceIntent)
        }
    }
}""",
        1,
    )
elif "LiveReplayForegroundService::class.java" not in activity_text:
    raise SystemExit("MainActivity.kt shape changed; foreground-service patch needs updating")

main_activity_path.write_text(activity_text, encoding="utf-8")

service_path = main_activity_path.parent / "LiveReplayForegroundService.kt"
service_path.write_text(
    f'''package {package_name}

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.os.Build
import android.os.IBinder
import android.os.PowerManager

class LiveReplayForegroundService : Service() {{
    companion object {{
        private const val CHANNEL_ID = "live_replay_recording"
        private const val NOTIFICATION_ID = 19159
    }}

    private var wakeLock: PowerManager.WakeLock? = null

    override fun onCreate() {{
        super.onCreate()
        createNotificationChannel()

        val powerManager = getSystemService(POWER_SERVICE) as PowerManager
        wakeLock = powerManager.newWakeLock(
            PowerManager.PARTIAL_WAKE_LOCK,
            "LiveReplay:RecorderWakeLock",
        ).apply {{
            setReferenceCounted(false)
            acquire()
        }}

        startForeground(NOTIFICATION_ID, buildNotification())
    }}

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {{
        return START_STICKY
    }}

    override fun onDestroy() {{
        wakeLock?.let {{ lock ->
            if (lock.isHeld) lock.release()
        }}
        wakeLock = null
        super.onDestroy()
    }}

    override fun onBind(intent: Intent?): IBinder? = null

    private fun createNotificationChannel() {{
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {{
            val channel = NotificationChannel(
                CHANNEL_ID,
                "Live Replay 后台录制",
                NotificationManager.IMPORTANCE_LOW,
            ).apply {{
                description = "保持直播录制与上传任务在锁屏后继续运行"
                setShowBadge(false)
            }}
            getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        }}
    }}

    private fun buildNotification(): Notification {{
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {{
            Notification.Builder(this, CHANNEL_ID)
        }} else {{
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }}

        return builder
            .setSmallIcon(applicationInfo.icon)
            .setContentTitle("Live Replay 正在后台运行")
            .setContentText("Huawei P40 / HarmonyOS 后台保护已启用")
            .setOngoing(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .build()
    }}
}}
''',
    encoding="utf-8",
)

print("Huawei P40 / HarmonyOS 4.2 tuning applied")
print(f"  manifest: {manifest_path}")
print(f"  activity: {main_activity_path}")
print(f"  service:  {service_path}")
for permission in required_permissions:
    print(f"  permission: {permission}")
