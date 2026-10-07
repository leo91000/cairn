package dev.leo.manager.ui

import android.os.Build
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import dev.leo.manager.data.*
import kotlinx.coroutines.CancellationException

val LocalLeoCredentials = staticCompositionLocalOf<LeoCredentials?> { null }

@Composable
fun accountCredentials(): LeoCredentials {
    val context = LocalContext.current
    return LocalLeoCredentials.current ?: remember(context) { AndroidLeoCredentials(context) }
}

@Composable
fun ProviderButtons(vm: LeoViewModel, state: Workspace, linking: Boolean = false) {
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
    listOf(
            "Google" to options.google,
            "GitHub" to options.github,
            "une passkey" to (options.passkeys && Build.VERSION.SDK_INT >= 28 && !linking),
        )
        .forEach { (provider, available) ->
            if (available)
                OutlinedButton(
                    onClick = { vm.perform { signIn(provider, credentials) } },
                    enabled = !state.busy,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(if (linking) "Associer $provider" else "Continuer avec $provider")
                }
        }
    if (state.signingIn) TextButton(onClick = vm::cancelSignIn) { Text("Annuler la connexion") }
}

@Composable
fun AccountSettings(vm: LeoViewModel, state: Workspace) {
    val credentials = accountCredentials()
    var methods by remember(state.session.account?.id) { mutableStateOf(AccountMethods()) }
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
        Text("Compte Leo", style = MaterialTheme.typography.titleLarge)
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
            if (methods.methods.any { it.kind == "passkey" })
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
        methods.methods.forEach { method ->
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
                enabled = !state.busy && methods.methods.size > 1,
            ) {
                Text("Retirer $name")
            }
        }
    }
}
