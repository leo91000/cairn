package build.cairn.app.data

import android.app.PendingIntent
import android.content.Context
import android.os.CancellationSignal
import androidx.credentials.*
import androidx.credentials.exceptions.*
import androidx.test.core.app.ApplicationProvider
import com.google.android.libraries.identity.googleid.GetSignInWithGoogleOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import java.util.concurrent.Executor
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [32])
class AndroidCairnCredentialsTest {
    @Test
    fun `SDK adapter binds Google nonce translates passkey proofs and clears provider state`() =
        runBlocking {
            val context = ApplicationProvider.getApplicationContext<Context>()
            var cleared = false
            var fail = false
            val manager =
                object : CredentialManager {
                    override fun getCredentialAsync(
                        context: Context,
                        request: GetCredentialRequest,
                        cancellationSignal: CancellationSignal?,
                        executor: Executor,
                        callback:
                            CredentialManagerCallback<
                                GetCredentialResponse,
                                GetCredentialException,
                            >,
                    ) {
                        if (fail) {
                            callback.onError(
                                GetCredentialUnknownException("private-provider-proof")
                            )
                            return
                        }
                        val option = request.credentialOptions.single()
                        val credential =
                            when (option) {
                                is GetSignInWithGoogleOption -> {
                                    assertEquals("server-client", option.serverClientId)
                                    assertEquals("one-use-nonce", option.nonce)
                                    CustomCredential(
                                        GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL,
                                        GoogleIdTokenCredential.Builder()
                                            .setId("alice@example.test")
                                            .setIdToken(
                                                "e30.eyJzdWIiOiJhbGljZSIsImVtYWlsIjoiYWxpY2VAZXhhbXBsZS50ZXN0In0.c2ln"
                                            )
                                            .build()
                                            .data,
                                    )
                                }
                                is GetPublicKeyCredentialOption -> {
                                    assertTrue(option.requestJson.contains("rpId"))
                                    PublicKeyCredential(
                                        """{"id":"key","response":{"signature":"signed"}}"""
                                    )
                                }
                                else -> error("Unexpected credential option")
                            }
                        callback.onResult(GetCredentialResponse(credential))
                    }

                    override fun createCredentialAsync(
                        context: Context,
                        request: CreateCredentialRequest,
                        cancellationSignal: CancellationSignal?,
                        executor: Executor,
                        callback:
                            CredentialManagerCallback<
                                CreateCredentialResponse,
                                CreateCredentialException,
                            >,
                    ) {
                        assertTrue(
                            (request as CreatePublicKeyCredentialRequest).requestJson.contains("rp")
                        )
                        callback.onResult(
                            CreatePublicKeyCredentialResponse(
                                """{"id":"created","response":{"attestationObject":"signed"}}"""
                            )
                        )
                    }

                    override fun clearCredentialStateAsync(
                        request: ClearCredentialStateRequest,
                        cancellationSignal: CancellationSignal?,
                        executor: Executor,
                        callback: CredentialManagerCallback<Void?, ClearCredentialException>,
                    ) {
                        cleared = true
                        callback.onResult(null)
                    }

                    override fun getCredentialAsync(
                        context: Context,
                        pendingGetCredentialHandle:
                            PrepareGetCredentialResponse.PendingGetCredentialHandle,
                        cancellationSignal: CancellationSignal?,
                        executor: Executor,
                        callback:
                            CredentialManagerCallback<
                                GetCredentialResponse,
                                GetCredentialException,
                            >,
                    ) = error("unused")

                    override fun prepareGetCredentialAsync(
                        request: GetCredentialRequest,
                        cancellationSignal: CancellationSignal?,
                        executor: Executor,
                        callback:
                            CredentialManagerCallback<
                                PrepareGetCredentialResponse,
                                GetCredentialException,
                            >,
                    ) = error("unused")

                    override fun createSettingsPendingIntent(): PendingIntent = error("unused")
                }
            val credentials = AndroidCairnCredentials(context, manager)
            assertEquals(
                "e30.eyJzdWIiOiJhbGljZSIsImVtYWlsIjoiYWxpY2VAZXhhbXBsZS50ZXN0In0.c2ln",
                credentials.google("server-client", "one-use-nonce"),
            )
            val options =
                """{"rpId":"cairn.example","challenge":"Y2hhbGxlbmdl","userVerification":"required"}"""
            assertTrue(credentials.authenticatePasskey(options).contains("signature"))
            assertTrue(
                credentials
                    .createPasskey(
                        """{"rp":{"id":"cairn.example","name":"Cairn"},"challenge":"Y2hhbGxlbmdl","user":{"id":"dXNlcg","name":"alice@example.test","displayName":"Alice"},"pubKeyCredParams":[{"type":"public-key","alg":-7}]}"""
                    )
                    .contains("attestationObject")
            )
            credentials.clear()
            assertTrue(cleared)
            fail = true
            val error = runCatching {
                credentials.google("server-client", "one-use-nonce")
            }
                .exceptionOrNull()
            assertTrue(error is IllegalStateException)
            assertFalse(error?.message.orEmpty().contains("private-provider-proof"))
        }
}
