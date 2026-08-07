package app.tauri.livereplayandroid

import android.app.Activity
import android.content.Intent
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.arthenica.ffmpegkit.FFmpegKit
import com.arthenica.ffmpegkit.ReturnCode
import com.google.android.gms.auth.api.identity.AuthorizationRequest
import com.google.android.gms.auth.api.identity.Identity
import com.google.android.gms.auth.api.identity.RevokeAccessRequest
import com.google.android.gms.common.api.Scope
import java.io.File

private const val YOUTUBE_UPLOAD_SCOPE = "https://www.googleapis.com/auth/youtube.upload"

@InvokeArg
class FinalizeMp4Args {
    var inputPath: String = ""
    var outputPath: String = ""
}

@TauriPlugin
class LiveReplayAndroidPlugin(private val activity: Activity) : Plugin(activity) {
    @Command
    fun authorizeYoutube(invoke: Invoke) {
        val intent = Intent(activity, YoutubeAuthorizationActivity::class.java)
        startActivityForResult(invoke, intent, "youtubeAuthorizationResult")
    }

    @ActivityCallback
    private fun youtubeAuthorizationResult(invoke: Invoke, result: ActivityResult) {
        val data = result.data
        if (result.resultCode != Activity.RESULT_OK || data == null) {
            invoke.reject(
                data?.getStringExtra(YoutubeAuthorizationActivity.EXTRA_ERROR)
                    ?: "Google/YouTube 授权未完成"
            )
            return
        }
        val token = data.getStringExtra(YoutubeAuthorizationActivity.EXTRA_ACCESS_TOKEN)
        if (token.isNullOrBlank()) {
            invoke.reject("Google/YouTube 授权没有返回 access token")
            return
        }
        invoke.resolve(JSObject().apply {
            put("authorized", true)
            put("accessToken", token)
            put("accountLabel", data.getStringExtra(YoutubeAuthorizationActivity.EXTRA_ACCOUNT_LABEL))
            put("expiresAtMillis", null)
        })
    }

    @Command
    fun cachedYoutubeAuth(invoke: Invoke) {
        val request = AuthorizationRequest.builder()
            .setRequestedScopes(listOf(Scope(YOUTUBE_UPLOAD_SCOPE)))
            .build()
        Identity.getAuthorizationClient(activity)
            .authorize(request)
            .addOnSuccessListener { result ->
                if (result.hasResolution() || result.accessToken.isNullOrBlank()) {
                    invoke.resolve(JSObject().apply {
                        put("authorized", false)
                        put("accessToken", null)
                        put("accountLabel", null)
                        put("expiresAtMillis", null)
                    })
                } else {
                    val account = result.toGoogleSignInAccount()
                    invoke.resolve(JSObject().apply {
                        put("authorized", true)
                        put("accessToken", result.accessToken)
                        put("accountLabel", account?.email ?: account?.displayName ?: "Google Account")
                        put("expiresAtMillis", null)
                    })
                }
            }
            .addOnFailureListener { error ->
                invoke.reject("读取 Google/YouTube 授权状态失败: ${error.message}")
            }
    }

    @Command
    fun logoutYoutube(invoke: Invoke) {
        val request = RevokeAccessRequest.builder()
            .setScopes(listOf(Scope(YOUTUBE_UPLOAD_SCOPE)))
            .build()
        Identity.getAuthorizationClient(activity)
            .revokeAccess(request)
            .addOnSuccessListener { invoke.resolve() }
            .addOnFailureListener { error ->
                invoke.reject("退出 Google/YouTube 授权失败: ${error.message}")
            }
    }

    @Command
    fun finalizeMp4(invoke: Invoke) {
        val args = invoke.parseArgs(FinalizeMp4Args::class.java)
        val input = File(args.inputPath)
        val output = File(args.outputPath)
        if (!input.isFile || input.length() <= 0L) {
            invoke.reject("待 finalize 的录像不存在或为空: ${input.absolutePath}")
            return
        }
        output.parentFile?.mkdirs()
        if (output.exists()) output.delete()

        val command = buildString {
            append("-hide_banner -loglevel warning -y ")
            append("-i ").append(shellQuote(input.absolutePath)).append(' ')
            append("-map 0 -c copy -movflags +faststart -f mp4 ")
            append(shellQuote(output.absolutePath))
        }

        FFmpegKit.executeAsync(command) { session ->
            if (!ReturnCode.isSuccess(session.returnCode)) {
                invoke.reject(
                    "MP4 finalize 失败 (code=${session.returnCode}): ${session.failStackTrace ?: session.output}"
                )
                return@executeAsync
            }
            if (!output.isFile || output.length() <= 0L) {
                invoke.reject("FFmpeg 返回成功，但最终 MP4 不存在或为空；原录像保留。")
                return@executeAsync
            }
            invoke.resolve(JSObject().apply {
                put("outputPath", output.absolutePath)
                put("bytes", output.length())
            })
        }
    }

    private fun shellQuote(value: String): String {
        return "'" + value.replace("'", "'\\''") + "'"
    }
}
