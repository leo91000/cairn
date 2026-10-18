@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package build.cairn.app.ui

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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.*
import build.cairn.app.data.*
import build.cairn.app.diagnostics.PerfConnection
import build.cairn.app.diagnostics.PerfScreen
import build.cairn.app.diagnostics.Performance

/** Top-level destinations reached from the dock; everything else is a pushed screen. */
private val topLevel = setOf("fil", "missions", "atelier")

/** Screens that take the whole height: no dock, no generic top bar. */
private fun focusedRoute(route: String) =
    route.startsWith("chat/") ||
        route.startsWith("new-chat") ||
        route.startsWith("run/") ||
        route.startsWith("fil/run/") ||
        route == "search"

internal fun dockSelection(route: String): String =
    when {
        route in topLevel -> route
        route.startsWith("chat/") ||
            route.startsWith("fil/run/") ||
            route.startsWith("new-chat") ||
            route == "search" -> "fil"
        route.startsWith("run/") -> "missions"
        else -> "atelier"
    }

@Composable
fun CairnApp(
    sharedUrl: String = "",
    consumedShare: () -> Unit = {},
    vm: CairnViewModel = viewModel(),
    targetChat: String = "",
    targetCacheScope: String = "",
    consumedTarget: () -> Unit = {},
    credentials: CairnCredentials? = null,
    targetAccount: String = "",
) {
    val state by vm.state.collectAsStateWithLifecycle()
    CompositionLocalProvider(
        LocalCairnCredentials provides credentials,
        LocalConnectionFeedback provides ConnectionFeedback(state.connectionLost, state.notice),
    ) {
        // Gives every screen, including those outside the workspace Scaffold, the theme's
        // background and content colors instead of black text on the window background.
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            CairnAppContent(
                state,
                sharedUrl,
                consumedShare,
                vm,
                targetChat,
                targetCacheScope,
                consumedTarget,
                targetAccount,
            )
        }
    }
}

@Composable
private fun CairnAppContent(
    state: Workspace,
    sharedUrl: String,
    consumedShare: () -> Unit,
    vm: CairnViewModel,
    targetChat: String,
    targetCacheScope: String,
    consumedTarget: () -> Unit,
    targetAccount: String,
) {
    PerformanceUpload(vm, state)
    SideEffect {
        Performance.connection =
            when {
                !state.session.authenticated || state.installation == null -> PerfConnection.NONE
                state.installation?.online != true -> PerfConnection.OFFLINE
                state.transportRoute == "direct" -> PerfConnection.DIRECT
                else -> PerfConnection.RELAY
            }
        if (!state.session.authenticated) Performance.screen = PerfScreen.STARTUP
    }
    val pushReenrollmentRequired by
        vm.notifications.nativeReenrollmentRequired.collectAsStateWithLifecycle(false)
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
    // Pages outside the workspace Scaffold each apply the system bar insets once themselves.
    if (state.restoringSession) {
        Box(Modifier.fillMaxSize().systemBarsPadding()) {
            Page {
                Heading(
                    "Connexion à Beacon",
                    "Votre session est conservée sur cet appareil.",
                )
                Text(state.error.orEmpty())
                Button(onClick = { vm.perform { retrySession() } }, enabled = !state.busy) {
                    Text("Réessayer")
                }
                if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
            }
        }
        return
    }
    if (!state.session.authenticated) {
        LoginScreen(vm, state)
        return
    }
    var approvedTarget by
        remember(targetChat, targetCacheScope, targetAccount) { mutableStateOf(false) }
    LaunchedEffect(targetChat, targetCacheScope, targetAccount, state.session.account?.id) {
        if (targetChat.isNotBlank()) {
            try {
                approvedTarget = vm.openNotification(targetCacheScope, targetAccount)
                if (!approvedTarget) {
                    vm.notify(
                        "Cette notification ne correspond plus à une installation accessible."
                    )
                    consumedTarget()
                }
            } catch (error: Exception) {
                vm.report(error)
                consumedTarget()
            }
        }
    }
    Poll(state.session.account?.id, 30_000) {
        if (!state.busy) {
            try {
                vm.refreshInstallations()
            } catch (e: Exception) {
                vm.report(e)
            }
        }
    }

    if (state.installation == null) {
        val context = LocalContext.current
        Box(Modifier.fillMaxSize().systemBarsPadding()) {
            Page {
                Heading(
                    "Aucune installation",
                    "Ajoutez ou revendiquez une installation depuis Beacon.",
                )
                Text(state.session.account?.email.orEmpty())
                Text("Les installations de votre compte Cairn apparaîtront ici.")
                Text(
                    "Obtenez la commande d’installation dans Beacon, ou associez une installation existante avec cairn claim."
                )
                Button(onClick = { browse(context, state.origin.trimEnd('/') + "/claim") }) {
                    Text("Ajouter une installation")
                }
                TextButton(onClick = { vm.perform { refreshInstallations() } }) {
                    Text("Actualiser")
                }
                AccountSettings(vm, state)
                NotificationSettings(vm)
                PerformanceSettings(vm)
                TextButton(onClick = { vm.perform { logout() } }) { Text("Se déconnecter") }
                state.error?.let { ErrorNotice(it, vm::clearMessage) }
            }
        }
        return
    }
    val installation = checkNotNull(state.installation)
    // No installation row above the workspace: each screen's own header shows it, and only the
    // workspace Scaffold (or the offline page below) applies the system bar insets.
    key(installation.id, installation.role, state.session.csrf) {
        if (!installation.online) {
            var connection by rememberSaveable { mutableStateOf(false) }
            Box(Modifier.fillMaxSize().systemBarsPadding()) {
                Page {
                    if (pushReenrollmentRequired) PushReenrollmentNotice()
                    InstallationHeader(state, openConnection = { connection = true })
                    ConnectionLostNotice(state.connectionLost)
                    Heading(
                        "Installation hors ligne",
                        "Les exécutions continuent sur votre installation. Réessayez lorsqu’elle sera connectée.",
                    )
                    TextButton(onClick = { vm.perform { refreshInstallations() } }) {
                        Text("Actualiser")
                    }
                    PerformanceSettings(vm)
                    TextButton(onClick = { vm.perform { logout() } }) { Text("Se déconnecter") }
                    state.error?.let { ErrorNotice(it, vm::clearMessage) }
                }
                SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter))
            }
            if (connection) ConnectionSheet(vm, state, close = { connection = false })
        } else
            WorkspaceApp(
                vm,
                state,
                snackbar,
                sharedUrl,
                consumedShare,
                if (approvedTarget) targetChat else "",
                targetCacheScope,
                consumedTarget,
                pushReenrollmentRequired,
            )
    }
}

@Composable
private fun PushReenrollmentNotice() {
    Text(
        NATIVE_PUSH_REENROLLMENT_MESSAGE,
        modifier = Modifier.padding(12.dp),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.error,
    )
}

@Composable
private fun WorkspaceApp(
    vm: CairnViewModel,
    state: Workspace,
    snackbar: SnackbarHostState,
    sharedUrl: String,
    consumedShare: () -> Unit,
    targetChat: String,
    targetCacheScope: String,
    consumedTarget: () -> Unit,
    pushReenrollmentRequired: Boolean,
) {
    val nav = rememberNavController()
    fun openConnections() {
        if (state.isOwner) nav.navigate("connections")
        else vm.notify("Demandez au propriétaire de reconnecter le compte.")
    }

    fun openSettings() = nav.navigate("settings") { launchSingleTop = true }

    val backStack by nav.currentBackStackEntryAsState()
    val route = backStack?.destination?.route ?: "fil"
    var detailScreen by remember(route) { mutableStateOf<PerfScreen?>(null) }
    val performanceScreen = detailScreen ?: PerfScreen.fromRoute(route)
    SideEffect { Performance.screen = performanceScreen }
    LaunchedEffect(sharedUrl) {
        if (sharedUrl.isNotBlank()) {
            if (state.isOwner) nav.navigate("authorize") { launchSingleTop = true }
            else {
                vm.notify("Cette action est réservée au propriétaire de l’installation.")
                consumedShare()
            }
        }
    }
    LaunchedEffect(targetChat, vm.api.cacheScope) {
        if (targetChat.isNotBlank() && targetCacheScope == vm.api.cacheScope) {
            nav.navigate("chat/${segment(targetChat)}") {
                // A pager may now display a different page from its route's starting id.
                val pager = nav.currentDestination?.route
                if (pager == "chat/{id}" || pager == "fil/run/{id}") {
                    popUpTo(pager) { inclusive = true }
                }
                launchSingleTop = true
            }
            consumedTarget()
        }
    }
    var focusedContent by remember(route) { mutableStateOf(false) }
    val focused = focusedContent || focusedRoute(route)
    val selected = dockSelection(route)
    val conversations = rememberFilConversations(vm, state)
    val activity = rememberFilActivity(vm)
    val feed =
        buildFeed(
            conversations.state?.chats.orEmpty().filter { it.lifecycle == "active" },
            activity.orEmpty(),
            state.tasks,
            vm.localReads,
        )
    val attention = filAttention(feed)
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
        LocalPerformanceScreen provides { detailScreen = it },
        LocalSnackbar provides snackbar,
        LocalAgentPortraits provides rememberAgentPortraits(vm, state),
        LocalFilSnapshot provides FilSnapshot(conversations, activity),
    ) {
        BoxWithConstraints(
            Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)
        ) {
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
                                CairnIcons.Plus,
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
                                icon = {
                                    FilNavigationBadge(
                                        if (d.route == "fil") attention else FilAttention()
                                    ) {
                                        Icon(d.icon, d.label)
                                    }
                                },
                                label = { Text(d.label) },
                                colors =
                                    NavigationRailItemDefaults.colors(
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
                                colors =
                                    TopAppBarDefaults.topAppBarColors(
                                        containerColor = MaterialTheme.colorScheme.background
                                    ),
                            )
                    },
                    bottomBar = {
                        if (!wide && !focused && !WindowInsets.isImeVisible)
                            CairnDock(
                                selected,
                                ::navigate,
                                ::create,
                                Modifier.navigationBarsPadding(),
                                attention,
                            )
                    },
                ) { padding ->
                    Column(Modifier.padding(padding)) {
                        if (pushReenrollmentRequired) PushReenrollmentNotice()
                        if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                        ConnectionLostNotice(state.connectionLost)
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
                                    openRun = { nav.navigate("fil/run/$it") },
                                    openConnections = ::openConnections,
                                    search = { nav.navigate("search") },
                                    openAccount = ::openSettings,
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
                                        nav.navigate("new-chat?agent=$agent&project=") {
                                            popUpTo("fil")
                                        }
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
                                    openFilRun = { nav.navigate("fil/run/$it") { popUpTo("fil") } },
                                    back = { nav.popBackStack() },
                                    create = { nav.navigate("new-chat") },
                                    openConnections = ::openConnections,
                                    openSettings = ::openSettings,
                                )
                            }
                            composable("new-chat?agent={agent}&project={project}") { entry ->
                                ChatScreen(
                                    vm,
                                    state,
                                    null,
                                    initialAgent =
                                        entry.arguments?.getString("agent")?.ifBlank { null }
                                            ?: MAIN_AGENT_ID,
                                    initialProject =
                                        entry.arguments?.getString("project").orEmpty(),
                                    openChat = { nav.navigate("chat/$it") { popUpTo("fil") } },
                                    openRun = { nav.navigate("run/$it") },
                                    back = { nav.popBackStack() },
                                    create = { nav.navigate("new-chat") },
                                    openConnections = ::openConnections,
                                    openSettings = ::openSettings,
                                )
                            }
                            composable("fil/run/{id}") { entry ->
                                FilRunScreen(
                                    vm,
                                    state,
                                    entry.arguments?.getString("id").orEmpty(),
                                    openChat = { nav.navigate("chat/$it") { popUpTo("fil") } },
                                    openRun = { nav.navigate("run/$it") },
                                    openFilRun = { nav.navigate("fil/run/$it") { popUpTo("fil") } },
                                    back = { nav.popBackStack() },
                                    create = { nav.navigate("new-chat") },
                                    openConnections = ::openConnections,
                                    openSettings = ::openSettings,
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
                                    nav.navigate("run/$it") {
                                        popUpTo("run/{id}") { inclusive = true }
                                    }
                                }
                            }
                            composable("atelier") {
                                AtelierScreen(vm, state) { nav.navigate(it) }
                            }
                            composable("personal-connections") {
                                PersonalPluginAccountsScreen(vm, state)
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
                            if (state.isOwner) composable("mcps") { McpsScreen(vm, state) }
                            composable("skills") { SkillsScreen(vm, state) }
                            if (state.isOwner)
                                composable("connections") {
                                    ConnectionsScreen(vm, state) { nav.navigate("run/$it") }
                                }
                            if (state.isOwner)
                                composable("plugins") {
                                    PluginsScreen(vm, state) {
                                        nav.navigate("plugins/${segment(it)}")
                                    }
                                }
                            if (state.isOwner)
                                composable("plugins/{id}") { entry ->
                                    PluginScreen(
                                        vm,
                                        state,
                                        entry.arguments?.getString("id").orEmpty(),
                                        openChat = { nav.navigate("chat/$it") },
                                    )
                                }
                            if (state.isOwner)
                                composable("nodes") {
                                    NodesScreen(vm, state) { nav.navigate("run/$it") }
                                }
                            composable("settings") { SettingsScreen(vm, state) }
                            if (state.isOwner)
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
private fun LoginScreen(vm: CairnViewModel, state: Workspace) {
    var email by rememberSaveable(state.origin) { mutableStateOf("") }
    // Verification codes never enter saved instance state.
    var code by remember(state.emailForCode) { mutableStateOf("") }
    Scaffold { padding ->
        Box(
            Modifier.padding(padding).imePadding().fillMaxSize(),
            contentAlignment = Alignment.Center,
        ) {
            Column(Modifier.widthIn(max = 520.dp)) {
                Page {
                    Wordmark()
                    Heading("Bienvenue dans Cairn", "Connectez-vous à votre compte Cairn.")
                    Panel {
                        if (state.origin.isBlank()) {
                            Text("Beacon n’est pas configuré dans cette version de l’application.")
                        } else if (state.emailForCode == null) {
                            OutlinedTextField(
                                email,
                                { email = it },
                                Modifier.fillMaxWidth(),
                                label = { Text("Adresse e-mail") },
                                singleLine = true,
                                keyboardOptions =
                                    androidx.compose.foundation.text.KeyboardOptions(
                                        keyboardType =
                                            androidx.compose.ui.text.input.KeyboardType.Email
                                    ),
                            )
                            Button(
                                onClick = { vm.perform { requestEmailCode(email) } },
                                enabled = email.isNotBlank() && !state.busy,
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Text("Recevoir un code")
                            }
                        } else {
                            Text("Un code a été envoyé à ${state.emailForCode}.")
                            OutlinedTextField(
                                code,
                                { code = it },
                                Modifier.fillMaxWidth(),
                                label = { Text("Code reçu par e-mail") },
                                singleLine = true,
                                keyboardOptions =
                                    androidx.compose.foundation.text.KeyboardOptions(
                                        keyboardType =
                                            androidx.compose.ui.text.input.KeyboardType.Number
                                    ),
                            )
                            Button(
                                onClick = {
                                    vm.perform {
                                        verifyEmailCode(code)
                                        code = ""
                                    }
                                },
                                enabled = code.isNotBlank() && !state.busy,
                                modifier = Modifier.fillMaxWidth(),
                            ) {
                                Text("Se connecter")
                            }
                            TextButton(
                                onClick = { vm.perform { requestEmailCode(state.emailForCode) } },
                                enabled = !state.busy,
                            ) {
                                Text("Renvoyer un code")
                            }
                            TextButton(onClick = vm::changeEmail, enabled = !state.busy) {
                                Text("Changer d’adresse e-mail")
                            }
                        }
                        if (state.emailForCode == null && state.origin.isNotBlank())
                            ProviderButtons(vm, state)
                        if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                        state.error?.let { ErrorNotice(it, vm::clearMessage) }
                    }
                    Text(
                        "Les exécutions continuent sur vos installations lorsque l’application est fermée."
                    )
                }
            }
        }
    }
}

@Composable
fun RunCard(run: Run, open: (String) -> Unit) {
    SignalCard(
        Modifier.fillMaxWidth(),
        onClick = { open(run.id) },
        padding = PaddingValues(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            RunStatusTile(runPresentationStatus(run))
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
            Status(runPresentationStatus(run))
        }
    }
}

/** Square status mark shared by run history rows. */
@Composable
internal fun RunStatusTile(status: String, size: androidx.compose.ui.unit.Dp = 34.dp) {
    val tint =
        when (status) {
            "succeeded" -> signal.success
            "failed",
            "interrupted" -> signal.attention
            "suspended-exclusive" -> signal.warning
            "running",
            "queued" -> MaterialTheme.colorScheme.primary
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
                "succeeded" -> CairnIcons.Check
                "failed",
                "interrupted" -> CairnIcons.Close
                "running",
                "queued" -> CairnIcons.Play
                else -> CairnIcons.Pause
            },
            null,
            Modifier.size(size * 0.45f),
            tint = tint,
        )
    }
}
