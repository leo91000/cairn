package build.cairn.app.ui

import android.os.Build
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.AccountSignInMethod
import kotlinx.coroutines.CancellationException

val LocalCairnCredentials = staticCompositionLocalOf<CairnCredentials?> { null }

@Composable
fun accountCredentials(): CairnCredentials {
    val context = LocalContext.current
    return LocalCairnCredentials.current ?: remember(context) { AndroidCairnCredentials(context) }
}

@Composable
fun ProviderButtons(
    vm: CairnViewModel,
    state: Workspace,
    linking: Boolean = false,
    onAvailabilityChanged: (Boolean) -> Unit = {},
) {
    val credentials = accountCredentials()
    var options by remember(state.origin) { mutableStateOf(AccountOptions()) }
    LaunchedEffect(state.origin) {
        if (state.origin.isNotBlank())
            try {
                options = vm.accountOptions()
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                /* Email sign-in remains available when provider discovery fails. */
            }
    }
    val passkeyAvailable = options.passkeys && Build.VERSION.SDK_INT >= 28 && !linking
    val hasAny = passkeyAvailable || options.google || options.github
    LaunchedEffect(hasAny) { onAvailabilityChanged(hasAny) }

    if (hasAny || state.signingIn) {
        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
            if (passkeyAvailable) {
                FilledTonalButton(
                    onClick = { vm.perform { signIn(AccountProvider.Passkey, credentials) } },
                    enabled = !state.busy,
                    shape = RoundedCornerShape(12.dp),
                    colors =
                        ButtonDefaults.filledTonalButtonColors(
                            containerColor = MaterialTheme.colorScheme.primaryContainer,
                            contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
                        ),
                    modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp),
                ) {
                    SocialProviderIcon(AccountProvider.Passkey)
                    Spacer(Modifier.width(10.dp))
                    Text("Continuer avec ${AccountProvider.Passkey.label}")
                }
            }
            listOf(
                    AccountProvider.Google to options.google,
                    AccountProvider.GitHub to options.github,
                )
                .forEach { (provider, available) ->
                    if (available) {
                        OutlinedButton(
                            onClick = { vm.perform { signIn(provider, credentials) } },
                            enabled = !state.busy,
                            shape = RoundedCornerShape(12.dp),
                            border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                            modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp),
                        ) {
                            SocialProviderIcon(provider)
                            Spacer(Modifier.width(10.dp))
                            Text(
                                if (linking) "Associer ${provider.label}"
                                else "Continuer avec ${provider.label}"
                            )
                        }
                    }
                }
            if (state.signingIn) {
                TextButton(
                    onClick = vm::cancelSignIn,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text("Annuler la connexion")
                }
            }
        }
    }
}

@Composable
fun AccountSettings(vm: CairnViewModel, state: Workspace) {
    val credentials = accountCredentials()
    var methods by
        remember(state.session.account?.id) { mutableStateOf(emptyList<AccountSignInMethod>()) }
    var label by remember { mutableStateOf("Téléphone") }
    var code by remember { mutableStateOf("") }
    var challenge by remember { mutableStateOf<String?>(null) }
    Poll("account-methods", 30_000) {
        try {
            methods = vm.accountMethods()
        } catch (error: Exception) {
            vm.report(error)
        }
    }
    Panel {
        Text("Compte Cairn", style = MaterialTheme.typography.titleLarge)
        Text(state.session.account?.email.orEmpty())
        ProviderButtons(vm, state, linking = true)
        Text(
            "Confirmez votre identité par e-mail ou avec une passkey avant de créer ou retirer une méthode de connexion."
        )
        TextButton(
            onClick = { vm.perform { challenge = accountConfirmationCode().challenge } },
            enabled = !state.busy,
        ) {
            Text("Recevoir un code de confirmation")
        }
        if (challenge != null) {
            OutlinedTextField(
                code,
                { code = it },
                label = { Text("Code de confirmation") },
                singleLine = true,
            )
            TextButton(
                onClick = {
                    vm.perform {
                        confirmAccountEmail(checkNotNull(challenge), code)
                        code = ""
                        challenge = null
                        notify("Identité confirmée.")
                    }
                },
                enabled = !state.busy && code.isNotBlank(),
            ) {
                Text("Confirmer mon identité")
            }
        }
        if (Build.VERSION.SDK_INT >= 28) {
            if (methods.any { it.kind == "passkey" })
                TextButton(
                    onClick = {
                        vm.perform {
                            confirmAccountPasskey(credentials)
                            notify("Identité confirmée.")
                        }
                    },
                    enabled = !state.busy,
                ) {
                    Text("Confirmer avec une passkey")
                }
            OutlinedTextField(
                label,
                { label = it.take(80) },
                label = { Text("Nom de la passkey") },
                singleLine = true,
            )
            TextButton(
                onClick = {
                    vm.perform {
                        createAccountPasskey(label, credentials)
                        methods = accountMethods()
                        notify("Passkey créée.")
                    }
                },
                enabled = !state.busy && label.isNotBlank(),
            ) {
                Text("Créer une passkey")
            }
        }
        methods.forEach { method ->
            val name =
                when (method.kind) {
                    "google" -> "Google"
                    "github" -> "GitHub"
                    "email" -> "E-mail"
                    "passkey" -> "Passkey"
                    else -> "Méthode de connexion"
                }
            Text("$name · ${method.label}")
            TextButton(
                onClick = {
                    vm.perform {
                        removeAccountMethod(method.id)
                        methods = accountMethods()
                    }
                },
                enabled = !state.busy && methods.size > 1,
            ) {
                Text("Retirer $name")
            }
        }
    }
}
