package build.cairn.app.ui

import build.cairn.contracts.CodingAgentSetting

/** Native property names stay verbatim in text mode; form copy follows the app language. */
internal fun CodingAgentSetting.inFrench(): CodingAgentSetting {
    val copy =
        when (path) {
            "/model_context_window" ->
                "Fenêtre de contexte" to
                    "Nombre de tokens disponibles. Cette valeur n’augmente pas la capacité réelle du modèle."
            "/model_auto_compact_token_limit" ->
                "Seuil de compaction automatique" to
                    "Résumer l’historique après ce nombre de tokens. Automatique utilise le seuil du modèle."
            "/tool_output_token_limit" ->
                "Limite des réponses d’outils" to
                    "Nombre maximal de tokens conservés par réponse d’outil."
            "/model_verbosity" ->
                "Détail des réponses" to "Niveau de détail préféré pour les modèles compatibles."
            "/web_search" ->
                "Recherche web" to "Résultats en direct, en cache, ou recherche native désactivée."
            "/features/multi_agent" ->
                "Sous-agents natifs" to
                    "Autoriser Codex à utiliser ses sous-agents dans les droits de l’exécution."
            "/alwaysThinkingEnabled" ->
                "Réflexion approfondie" to "Activer la réflexion approfondie par défaut."
            "/language" -> "Langue des réponses" to "Langue préférée pour les réponses de Claude."
            "/outputStyle" ->
                "Style des réponses" to
                    "Nom d’un style disponible dans l’environnement d’exécution."
            "/env/CLAUDE_AUTOCOMPACT_PCT_OVERRIDE" ->
                "Pourcentage de compaction" to
                    "Pourcentage natif écrit sous forme de texte, par exemple 80."
            "/env/CLAUDE_CODE_MAX_OUTPUT_TOKENS" ->
                "Longueur maximale des réponses" to
                    "Budget de sortie natif en tokens, écrit sous forme de texte."
            "/snapshot" ->
                "Snapshots de l’espace de travail" to "Activer les snapshots natifs d’OpenCode."
            "/logLevel" ->
                "Niveau des journaux natifs" to "Niveau de détail des diagnostics du moteur."
            "/default_agent" ->
                "Agent natif" to "Nom d’un agent OpenCode disponible dans l’exécution."
            "/model" -> "Modèle" to "Même valeur que dans les réglages de l’agent Cairn."
            "/model_reasoning_effort",
            "/effortLevel" ->
                "Effort de raisonnement" to "Même valeur que dans les réglages de l’agent Cairn."
            "/developer_instructions" ->
                "Instructions supplémentaires" to
                    "Mêmes instructions que sur la page de l’agent Cairn."
            "/service_tier" ->
                "Vitesse" to
                    "Valeur native : default pour Standard, ou le nom d’un niveau proposé par le compte."
            else -> label to description
        }
    return copy(
        label = copy.first,
        description = copy.second,
        group =
            when (group) {
                "Context" -> "Contexte"
                "Responses" -> "Réponses"
                "Tools" -> "Outils"
                "Workspace" -> "Espace de travail"
                "Agent defaults" -> "Valeurs de l’agent"
                else -> group
            },
    )
}

internal fun nativeManagedReason(path: String): String =
    when (path) {
        "credentials" ->
            "Les comptes et leurs identifiants se gèrent dans Connexions et ne figurent jamais dans ce document."
        "permissions" ->
            "Les droits accordés dans Cairn restent le plafond. Modifiez le mode d’exécution sur la page de l’agent."
        "mcp" -> "Cairn fournit les outils MCP autorisés et les jetons privés de l’exécution."
        "notify" ->
            "Les commandes de notification natives ne peuvent pas s’exécuter hors des contrôles de droits Cairn. Utilisez les skills et les plugins Cairn."
        "hooks" ->
            "Les commandes natives de cycle de vie et leur état de confiance ne sont pas pris en charge. Utilisez les skills et les plugins Cairn."
        "features.default_mode_request_user_input" ->
            "Cairn fournit les questionnaires. L’outil de question natif de Codex en mode par défaut reste désactivé."
        "hooks / command helpers" ->
            "Les hooks et commandes d’authentification natifs ne sont pas pris en charge. Utilisez les skills, plugins et Connexions Cairn."
        "env" ->
            "Seuls CLAUDE_AUTOCOMPACT_PCT_OVERRIDE, CLAUDE_CODE_MAX_OUTPUT_TOKENS et MAX_THINKING_TOKENS sont modifiables. Cairn gère l’environnement d’exécution."
        "configuration substitutions" ->
            "Les substitutions de fichiers et de variables sont refusées dans les clés et les valeurs. Utilisez des valeurs littérales vérifiables avant l’exécution."
        "agent / mode permissions" ->
            "Les droits et outils des agents natifs, y compris les anciens modes, ne peuvent pas remplacer la sélection d’outils accordée dans Cairn. Modifiez les accès sur la page de l’agent Cairn."
        "formatter / lsp commands" ->
            "Les commandes personnalisées de formatage et de serveurs de langage, ainsi que leur environnement, ne sont pas prises en charge. Utilisez les skills et plugins Cairn."
        "features.plugins",
        "enabledPlugins",
        "plugin" ->
            "Les plugins natifs ne sont pas pris en charge. Utilisez les skills et les plugins Cairn."
        "compaction.auto" ->
            "La compaction automatique d’OpenCode n’est pas compatible avec la reprise des sessions dans Cairn."
        "autoupdate / share" ->
            "Cairn utilise des versions validées du moteur et désactive le partage natif."
        else -> path
    }
