package build.cairn.app.data

import android.content.Context
import androidx.browser.customtabs.CustomTabsIntent
import androidx.core.net.toUri
import androidx.credentials.ClearCredentialStateRequest
import androidx.credentials.CreatePublicKeyCredentialRequest
import androidx.credentials.CreatePublicKeyCredentialResponse
import androidx.credentials.CredentialManager
import androidx.credentials.CustomCredential
import androidx.credentials.GetCredentialRequest
import androidx.credentials.GetPublicKeyCredentialOption
import androidx.credentials.PublicKeyCredential
import com.google.android.libraries.identity.googleid.GetSignInWithGoogleOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import kotlinx.coroutines.CancellationException

/** Android SDK adapter; provider results are never persisted and only Cairn exchanges them. */
class AndroidCairnCredentials(
    private val context: Context,
    private val manager: CredentialManager = CredentialManager.create(context.applicationContext),
) : CairnCredentials {

    override suspend fun google(clientId: String, nonce: String): String = credential {
        val option = GetSignInWithGoogleOption.Builder(clientId).setNonce(nonce).build()
        val result = manager.getCredential(context, GetCredentialRequest(listOf(option)))
        val credential = result.credential
        require(
            credential is CustomCredential &&
                credential.type == GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL
        ) {
            "Google n’a pas renvoyé une identité valide."
        }
        GoogleIdTokenCredential.createFrom(credential.data).idToken
    }

    override suspend fun authenticatePasskey(options: String): String = credential {
        val result =
            manager.getCredential(
                context,
                GetCredentialRequest(listOf(GetPublicKeyCredentialOption(options))),
            )
        (result.credential as? PublicKeyCredential)?.authenticationResponseJson
            ?: error("Aucune passkey valide n’a été reçue.")
    }

    override suspend fun createPasskey(options: String): String = credential {
        val result = manager.createCredential(context, CreatePublicKeyCredentialRequest(options))
        (result as? CreatePublicKeyCredentialResponse)?.registrationResponseJson
            ?: error("La passkey n’a pas pu être créée.")
    }

    override fun openGitHub(url: String) {
        try {
            CustomTabsIntent.Builder().build().launchUrl(context, url.toUri())
        } catch (_: Exception) {
            error("Ouvrez un navigateur Android pour vous connecter à GitHub.")
        }
    }

    private suspend fun <T> credential(block: suspend () -> T): T =
        try {
            block()
        } catch (error: CancellationException) {
            throw error
        } catch (_: androidx.credentials.exceptions.GetCredentialCancellationException) {
            throw CancellationException("Connexion annulée.")
        } catch (_: androidx.credentials.exceptions.CreateCredentialCancellationException) {
            throw CancellationException("Création annulée.")
        } catch (_: Exception) {
            error(
                "Le fournisseur d’identité est indisponible. Réessayez ou utilisez un code par e-mail."
            )
        }

    override suspend fun clear() {
        try {
            manager.clearCredentialState(ClearCredentialStateRequest())
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            // Clearing the local Cairn session must succeed even if a provider is unavailable.
        }
    }
}
