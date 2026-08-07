package app.tauri.livereplayandroid

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import com.google.android.gms.auth.api.identity.AuthorizationRequest
import com.google.android.gms.auth.api.identity.AuthorizationResult
import com.google.android.gms.auth.api.identity.Identity
import com.google.android.gms.common.api.Scope

class YoutubeAuthorizationActivity : Activity() {
    companion object {
        private const val REQUEST_AUTHORIZATION = 9021
        const val EXTRA_ACCESS_TOKEN = "access_token"
        const val EXTRA_ACCOUNT_LABEL = "account_label"
        const val EXTRA_ERROR = "error"
        private const val YOUTUBE_UPLOAD_SCOPE = "https://www.googleapis.com/auth/youtube.upload"
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) {
            beginAuthorization()
        }
    }

    private fun beginAuthorization() {
        val request = AuthorizationRequest.builder()
            .setRequestedScopes(listOf(Scope(YOUTUBE_UPLOAD_SCOPE)))
            .build()
        val client = Identity.getAuthorizationClient(this)
        client.authorize(request)
            .addOnSuccessListener { result ->
                if (result.hasResolution()) {
                    val pendingIntent = result.pendingIntent
                    if (pendingIntent == null) {
                        finishWithError("Google 授权需要交互，但没有返回 PendingIntent")
                        return@addOnSuccessListener
                    }
                    try {
                        startIntentSenderForResult(
                            pendingIntent.intentSender,
                            REQUEST_AUTHORIZATION,
                            null,
                            0,
                            0,
                            0
                        )
                    } catch (error: Exception) {
                        finishWithError("启动 Google 授权界面失败: ${error.message}")
                    }
                } else {
                    finishWithResult(result)
                }
            }
            .addOnFailureListener { error ->
                finishWithError("Google/YouTube 授权失败: ${error.message}")
            }
    }

    @Deprecated("Deprecated in Android framework; required for Google PendingIntent result handling")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQUEST_AUTHORIZATION) return
        if (resultCode != RESULT_OK || data == null) {
            finishWithError("Google/YouTube 授权被取消")
            return
        }
        try {
            val result = Identity.getAuthorizationClient(this)
                .getAuthorizationResultFromIntent(data)
            finishWithResult(result)
        } catch (error: Exception) {
            finishWithError("读取 Google/YouTube 授权结果失败: ${error.message}")
        }
    }

    private fun finishWithResult(result: AuthorizationResult) {
        val token = result.accessToken
        if (token.isNullOrBlank()) {
            finishWithError("Google 已授权，但没有返回 YouTube access token")
            return
        }
        val account = result.toGoogleSignInAccount()
        val label = account?.email ?: account?.displayName ?: "Google Account"
        setResult(
            RESULT_OK,
            Intent().apply {
                putExtra(EXTRA_ACCESS_TOKEN, token)
                putExtra(EXTRA_ACCOUNT_LABEL, label)
            }
        )
        finish()
    }

    private fun finishWithError(message: String) {
        setResult(
            RESULT_CANCELED,
            Intent().apply { putExtra(EXTRA_ERROR, message) }
        )
        finish()
    }
}
