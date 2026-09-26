@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package dev.leo.manager.ui

import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleOut
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.*
import dev.leo.manager.data.*

/** Top-level destinations reached from the dock; everything else is a pushed screen. */
private val topLevel = setOf("fil", "missions", "atelier")

/** Screens that take the whole height: no dock, no generic top bar. */
private fun focusedRoute(route: String) =
    route.startsWith("chat/") || route.startsWith("new-chat") || route.startsWith("run/") || route == "search"

internal fun dockSelection(route: String): String =
    when {
        route in topLevel -> route
        route.startsWith("chat/") || route.startsWith("new-chat") || route == "search" -> "fil"
        route.startsWith("run/") -> "missions"
        else -> "atelier"
    }

@Composable
fun LeoApp(
    sharedUrl: String = "",
    consumedShare: () -> Unit = {},
    vm: LeoViewModel = viewModel(),
    targetChat: String = "",
    targetOrigin: String = "",
    consumedTarget: () -> Unit = {},
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(state.notice) {
        state.notice?.let {
            snackbar.showSnackbar(it)
            vm.clearNotice()
        }
    }
    if (!state.ready) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        return
    }
    if (!state.session.authenticated) {
        LoginScreen(vm, state)
        return
    }
    val nav = rememberNavController()
    val backStack by nav.currentBackStackEntryAsState()
    val route = backStack?.destination?.route ?: "fil"
    LaunchedEffect(sharedUrl) {
        if (sharedUrl.isNotBlank()) nav.navigate("authorize") { launchSingleTop = true }
    }
    LaunchedEffect(targetChat, state.origin) {
        if (targetChat.isNotBlank() && targetOrigin == state.origin) {
            nav.navigate("chat/${segment(targetChat)}") {
                // A pager may now display a different chat from its route's starting id.
                if (nav.currentDestination?.route == "chat/{id}") {
                    popUpTo("chat/{id}") { inclusive = true }
                }
                launchSingleTop = true
            }
            consumedTarget()
        }
    }
    var focusedContent by remember(route) { mutableStateOf(false) }
    val focused = focusedContent || focusedRoute(route)
    val selected = dockSelection(route)
    fun navigate(target: String) {
        nav.navigate(target) {
            popUpTo("fil") { saveState = true }
            launchSingleTop = true
            restoreState = true
        }
    }
    fun create() = nav.navigate("new-chat") { launchSingleTop = true }
    CompositionLocalProvider(
        LocalFocusMode provides { focusedContent = it },
        LocalSnackbar provides snackbar,
        LocalAgentPortraits provides rememberAgentPortraits(vm, state),
    ) {
        BoxWithConstraints(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) {
            val wide = maxWidth >= 700.dp
            Row {
                if (wide && !focusedContent)
                    NavigationRail(
                        Modifier.fillMaxHeight(),
                        containerColor = MaterialTheme.colorScheme.background,
                        header = {
                            Spacer(Modifier.height(16.dp))
                            RoundAction(
                                "Nouvelle conversation",
                                LeoIcons.Plus,
                                container = MaterialTheme.colorScheme.primary,
                                content = MaterialTheme.colorScheme.onPrimary,
                                outlined = false,
                                size = 52.dp,
                                onClick = ::create,
                            )
                            Spacer(Modifier.height(8.dp))
                        },
                    ) {
                        DockItems.forEach { d ->
                            NavigationRailItem(
                                selected == d.route,
                                { navigate(d.route) },
                                icon = { Icon(d.icon, d.label) },
                                label = { Text(d.label) },
                                colors = NavigationRailItemDefaults.colors(
                                    indicatorColor = MaterialTheme.colorScheme.primary,
                                    selectedIconColor = MaterialTheme.colorScheme.onPrimary,
                                    selectedTextColor = MaterialTheme.colorScheme.primary,
                                ),
                            )
                        }
                    }
                Scaffold(
                    modifier = Modifier.weight(1f).imePadding(),
                    containerColor = MaterialTheme.colorScheme.background,
                    snackbarHost = { SnackbarHost(snackbar) },
                    topBar = {
                        if (!focused && route !in topLevel)
                            TopAppBar(
                                title = {},
                                navigationIcon = {
                                    IconButton(onClick = { nav.popBackStack() }) {
                                        Icon(Icons.AutoMirrored.Filled.ArrowBack, "Retour")
                                    }
                                },
                                actions = {
                                    IconButton(
                                        onClick = { vm.perform { refresh() } },
                                        enabled = !state.busy,
                                    ) {
                                        Icon(Icons.Default.Refresh, "Actualiser l’espace")
                                    }
                                },
                                colors = TopAppBarDefaults.topAppBarColors(
                                    containerColor = MaterialTheme.colorScheme.background,
                                ),
                            )
                    },
                    bottomBar = {
                        if (!wide && !focused && !WindowInsets.isImeVisible)
                            LeoDock(
                                selected,
                                ::navigate,
                                ::create,
                                Modifier.navigationBarsPadding(),
                            )
                    },
                ) { padding ->
                    Column(Modifier.padding(padding)) {
                        if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                        state.error?.let { ErrorNotice(it, vm::clearMessage) }
                        NavHost(
                            nav,
                            "fil",
                            Modifier.weight(1f),
                            // The default predictive back only scales the leaving screen, so its
                            // content stayed opaque while the screen below faded in.
                            predictivePopExitTransition = {
                                scaleOut(targetScale = 0.7f) + fadeOut(tween(700))
                            },
                        ) {
                            composable("fil") {
                                FilScreen(
                                    vm,
                                    state,
                                    openChat = { nav.navigate("chat/$it") },
                                    openRun = { nav.navigate("run/$it") },
                                    openConnections = { nav.navigate("connections") },
                                    search = { nav.navigate("search") },
                                )
                            }
                            composable("search") {
                                SearchScreen(
                                    vm,
                                    state,
                                    back = { nav.popBackStack() },
                                    openChat = { nav.navigate("chat/$it") { popUpTo("fil") } },
                                    openRun = { nav.navigate("run/$it") { popUpTo("fil") } },
                                    openMission = { navigate("missions") },
                                    newChat = { agent ->
                                        nav.navigate("new-chat?agent=$agent&project=") { popUpTo("fil") }
                                    },
                                    open = { nav.navigate(it) { popUpTo("fil") } },
                                )
                            }
                            composable("chat/{id}") { entry ->
                                ChatScreen(
                                    vm,
                                    state,
                                    entry.arguments?.getString("id"),
                                    openChat = { nav.navigate("chat/$it") { popUpTo("fil") } },
                                    openRun = { nav.navigate("run/$it") },
                                    back = { nav.popBackStack() },
                                    create = { nav.navigate("new-chat") },
                                    openConnections = { nav.navigate("connections") },
                                )
                            }
                            composable("new-chat?agent={agent}&project={project}") { entry ->
                                ChatScreen(
                                    vm,
                                    state,
                                    null,
                                    initialAgent =
                                        entry.arguments?.getString("agent")?.ifBlank { null } ?: MAIN_AGENT_ID,
                                    initialProject =
                                        entry.arguments?.getString("project").orEmpty(),
                                    openChat = { nav.navigate("chat/$it") { popUpTo("fil") } },
                                    openRun = { nav.navigate("run/$it") },
                                    back = { nav.popBackStack() },
                                    create = { nav.navigate("new-chat") },
                                    openConnections = { nav.navigate("connections") },
                                )
                            }
                            composable("missions") {
                                MissionsScreen(vm, state) { nav.navigate("run/$it") }
                            }
                            composable("runs") { RunsScreen(vm, state) { nav.navigate("run/$it") } }
                            composable("run/{id}") { entry ->
                                RunScreen(
                                    vm,
                                    state,
                                    entry.arguments?.getString("id").orEmpty(),
                                    openChat = { nav.navigate("chat/$it") },
                                    back = { nav.popBackStack() },
                                ) {
                                    nav.navigate("run/$it") { popUpTo("run/{id}") { inclusive = true } }
                                }
                            }
                            composable("atelier") {
                                AtelierScreen(vm, state) { nav.navigate(it) }
                            }
                            composable("agents") {
                                ResourcesScreen(vm, state, true) { agent, project ->
                                    nav.navigate("new-chat?agent=$agent&project=$project")
                                }
                            }
                            composable("projects") {
                                ResourcesScreen(vm, state, false) { agent, project ->
                                    nav.navigate("new-chat?agent=$agent&project=$project")
                                }
                            }
                            composable("mcps") { McpsScreen(vm, state) }
                            composable("skills") { SkillsScreen(vm, state) }
                            composable("connections") {
                                ConnectionsScreen(vm, state) { nav.navigate("run/$it") }
                            }
                            composable("nodes") { NodesScreen(vm, state) }
                            composable("settings") { SettingsScreen(vm, state) }
                            composable("authorize") {
                                AuthorizeScreen(vm, state, sharedUrl, consumedShare)
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun LoginScreen(vm: LeoViewModel, state: Workspace) {
    var origin by rememberSaveable(state.origin) { mutableStateOf(state.origin) }
    // Passwords and bootstrap tokens deliberately never enter saved instance state.
    var password by remember { mutableStateOf("") }
    var setupToken by remember { mutableStateOf("") }
    Scaffold { padding ->
        Box(
            Modifier.padding(padding).imePadding().fillMaxSize(),
            contentAlignment = Alignment.Center,
        ) {
            Column(Modifier.widthIn(max = 520.dp)) {
                Page {
                    Wordmark()
                    Heading("Bienvenue dans Leo", "Votre espace de travail, dans votre poche.")
                    Panel {
                        Text(
                            "Connexion à votre serveur",
                            style = MaterialTheme.typography.titleLarge,
                        )
                        if (state.origin.isBlank()) {
                            OutlinedTextField(
                                origin,
                                { origin = it },
                                Modifier.fillMaxWidth(),
                                label = { Text("Adresse du serveur") },
                                placeholder = { Text("https://leo.exemple.fr") },
                                singleLine = true,
                                keyboardOptions = InputKeyboards.Uri,
                            )
                            Button(
                                onClick = { vm.perform { connect(origin) } },
                                enabled = origin.isNotBlank() && !state.busy,
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Text("Continuer")
                            }
                        } else {
                            Text(state.origin, color = MaterialTheme.colorScheme.primary)
                            if (state.session.setupRequired) {
                                Text(
                                    "Créez le compte propriétaire avec le jeton d’installation du serveur."
                                )
                                OutlinedTextField(
                                    setupToken,
                                    { setupToken = it },
                                    Modifier.fillMaxWidth(),
                                    label = { Text("Jeton d’installation") },
                                    keyboardOptions = InputKeyboards.Password,
                                    visualTransformation = PasswordVisualTransformation(),
                                    singleLine = true,
                                )
                            }
                            OutlinedTextField(
                                password,
                                { password = it },
                                Modifier.fillMaxWidth(),
                                label = {
                                    Text(
                                        if (state.session.setupRequired)
                                            "Mot de passe · 12 caractères minimum"
                                        else "Mot de passe"
                                    )
                                },
                                visualTransformation = PasswordVisualTransformation(),
                                singleLine = true,
                                keyboardOptions = InputKeyboards.Password,
                            )
                            Button(
                                onClick = {
                                    vm.perform {
                                        login(password, setupToken)
                                        password = ""
                                        setupToken = ""
                                    }
                                },
                                enabled =
                                    !state.busy &&
                                        password.isNotBlank() &&
                                        (!state.session.setupRequired ||
                                            (password.length >= 12 && setupToken.isNotBlank())),
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Text(
                                    if (state.session.setupRequired) "Créer mon compte"
                                    else "Se connecter"
                                )
                            }
                            TextButton(
                                onClick = {
                                    vm.perform { forget() }
                                    password = ""
                                    setupToken = ""
                                },
                                enabled = !state.busy,
                            ) {
                                Text("Changer de serveur")
                            }
                        }
                        if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                        state.error?.let { ErrorNotice(it, vm::clearMessage) }
                    }
                    Text(
                        "Retrouvez vos agents, vos tâches et leurs résultats. Les exécutions continuent sur votre serveur.",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
fun RunCard(run: Run, open: (String) -> Unit) {
    SignalCard(Modifier.fillMaxWidth(), onClick = { open(run.id) }, padding = PaddingValues(14.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            RunStatusTile(run.status)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    run.title,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 2,
                    overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                )
                Text(
                    "${date(run.createdAt)} · ${duration(run)}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.width(8.dp))
            Status(run.status)
        }
    }
}

/** Square status mark shared by run history rows. */
@Composable
internal fun RunStatusTile(status: String, size: androidx.compose.ui.unit.Dp = 34.dp) {
    val tint =
        when (status) {
            "succeeded" -> signal.success
            "failed", "interrupted" -> signal.attention
            "running", "queued" -> MaterialTheme.colorScheme.primary
            else -> MaterialTheme.colorScheme.onSurfaceVariant
        }
    Box(
        Modifier.size(size)
            .clip(androidx.compose.foundation.shape.RoundedCornerShape(10.dp))
            .background(tint.copy(alpha = 0.14f)),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            when (status) {
                "succeeded" -> LeoIcons.Check
                "failed", "interrupted" -> LeoIcons.Close
                "running", "queued" -> LeoIcons.Play
                else -> LeoIcons.Pause
            },
            null,
            Modifier.size(size * 0.45f),
            tint = tint,
        )
    }
}
