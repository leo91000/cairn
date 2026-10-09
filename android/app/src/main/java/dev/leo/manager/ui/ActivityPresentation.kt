package dev.leo.manager.ui

import dev.leo.manager.data.*
import kotlinx.serialization.json.*

internal enum class ActivityKind(val label: String) {
    COMMAND("Terminal"),
    READ("Lecture"),
    BROWSE("Exploration"),
    SEARCH("Recherche"),
    FILES("Modifications"),
    TOOL("Outil"),
    PLAN("Plan"),
    THINKING("Réflexion"),
    NOTICE("Session"),
    OUTPUT("Résultat"),
}

internal data class ActivityPresentation(
    val kind: ActivityKind,
    val title: String,
    val subtitle: String = "",
    val state: String = "Terminé",
    val failed: Boolean = false,
    val command: String = "",
    val paths: List<String> = emptyList(),
    val output: String = "",
    val item: JsonObject? = null,
    val data: JsonObject? = null,
    val raw: String = "",
    val exitCode: Int? = null,
)

internal val activityJson = Json(wireJson) { prettyPrint = true }

internal fun JsonElement?.asObject() = this as? JsonObject

internal fun JsonElement?.asArray() = this as? JsonArray

internal fun JsonElement?.plain(): String = (this as? JsonPrimitive)?.contentOrNull.orEmpty()

internal fun RunEvent.activityData(): JsonObject? =
    payload?.let(::JsonObject)
        ?: runCatching {
            if (text.firstOrNull { !it.isWhitespace() } != '{') return@runCatching null
            (wireJson.parseToJsonElement(text) as? JsonObject)?.takeIf {
                it.string("type") == type || it.containsKey("item")
            }
        }
            .getOrNull()

internal fun readableField(value: String): String =
    value
        .replace(Regex("([a-z0-9])([A-Z])"), "$1 $2")
        .replace(Regex("[_.-]+"), " ")
        .replaceFirstChar { it.uppercase() }

internal fun friendlyStatus(value: String): String =
    when (value.lowercase()) {
        "succeeded",
        "success",
        "passed" -> "Réussi"
        "completed",
        "done" -> "Terminé"
        "failed",
        "failure",
        "error",
        "timed_out",
        "action_required" -> "Échec"
        "running",
        "in_progress",
        "in progress" -> "En cours"
        "queued",
        "pending",
        "waiting" -> "En attente"
        "cancelled",
        "canceled",
        "interrupted" -> "Interrompu"
        "skipped" -> "Ignoré"
        "merged" -> "Fusionné"
        "open" -> "Ouvert"
        "closed" -> "Fermé"
        else -> readableField(value)
    }

/** Lex only literal shell words, never evaluate expansions or compound commands. */
internal fun shellWords(source: String): List<String>? {
    val result = mutableListOf<String>()
    val word = StringBuilder()
    var quote: Char? = null
    var started = false
    var i = 0
    while (i < source.length) {
        val c = source[i++]
        if (quote == '\'') {
            if (c == quote) quote = null else word.append(c)
            continue
        }
        if (c == '\\') {
            if (i == source.length || source[i] == '\n') return null
            val next = source[i++]
            if (quote == '"' && next !in "$`\"\\") word.append('\\')
            word.append(next)
            started = true
            continue
        }
        if (c == '$' || c == '`') return null
        if (quote != null) {
            if (c == quote) quote = null else word.append(c)
            continue
        }
        if (c == '\'' || c == '"') {
            quote = c
            started = true
            continue
        }
        if (c in ";&|<>\n(){}*?[]" || (c == '#' && !started)) return null
        if (c.isWhitespace()) {
            if (started) result.add(word.toString())
            word.clear()
            started = false
        } else {
            word.append(c)
            started = true
        }
    }
    if (quote != null) return null
    if (started) result.add(word.toString())
    return result
}

internal data class CommandView(
    val kind: ActivityKind,
    val title: String,
    val source: String,
    val words: List<String>?,
    val paths: List<String> = emptyList(),
)

internal fun commandView(command: String): CommandView {
    var source = command
    var words = shellWords(source)
    if (
        words?.size == 3 &&
            words[0].substringAfterLast('/') in listOf("sh", "bash", "zsh") &&
            words[1] in listOf("-c", "-lc", "-cl")
    ) {
        source = words[2]
        words = shellWords(source)
    }
    val base = CommandView(ActivityKind.COMMAND, "Exécuter une commande", source, words)
    val name = words?.firstOrNull()?.substringAfterLast('/') ?: return base
    val args = words.drop(1)
    val paths =
        when {
            name == "cat" &&
                args.isNotEmpty() &&
                args.all { it.isNotEmpty() && (!it.startsWith('-') || it == "--") } ->
                args.filter { it != "--" }
            name == "sed" &&
                args.size == 3 &&
                args[0] == "-n" &&
                Regex("\\d+(?:,(?:\\d+|\\$))?p").matches(args[1]) &&
                !args[2].startsWith('-') -> listOf(args[2])
            name in listOf("head", "tail") && args.size == 1 && !args[0].startsWith('-') -> args
            name in listOf("head", "tail") &&
                args.size == 3 &&
                args[0] == "-n" &&
                Regex("\\+?\\d+").matches(args[1]) &&
                !args[2].startsWith('-') -> listOf(args[2])
            else -> emptyList()
        }
    if (paths.isNotEmpty())
        return base.copy(
            kind = ActivityKind.READ,
            title =
                if (paths.size == 1) "Lire ${paths[0].substringAfterLast('/')}"
                else "Lire ${paths.size} fichiers",
            paths = paths,
        )
    if (name in listOf("rg", "grep"))
        return base.copy(
            kind = if ("--files" in args) ActivityKind.BROWSE else ActivityKind.SEARCH,
            title =
                if ("--files" in args) "Parcourir les fichiers" else "Rechercher dans les fichiers",
        )
    if (name in listOf("ls", "fd", "find", "pwd"))
        return base.copy(kind = ActivityKind.BROWSE, title = "Explorer l’espace de travail")
    if (name == "git")
        return base.copy(
            title =
                mapOf(
                    "status" to "État du dépôt Git",
                    "diff" to "Examiner les différences",
                    "log" to "Historique des commits",
                    "show" to "Inspecter un objet Git",
                    "fetch" to "Actualiser le dépôt",
                    "commit" to "Créer un commit",
                    "push" to "Publier les commits",
                )[args.firstOrNull()] ?: "Commande Git"
        )
    if (name in listOf("pnpm", "npm", "yarn", "bun", "cargo", "gradlew", "gradle")) {
        val action = if (args.firstOrNull() == "run") args.getOrNull(1) else args.firstOrNull()
        val title =
            when {
                action?.startsWith("test") == true -> "Exécuter les tests"
                action in listOf("check", "typecheck") -> "Vérifier le projet"
                action == "lint" -> "Analyser le code"
                action in listOf("build", "assembleDebug", "assembleRelease") ->
                    "Compiler le projet"
                else -> base.title
            }
        return base.copy(title = title)
    }
    return base
}

internal fun expectedOutcome(view: CommandView, code: Int?): String? {
    if (code != 1) return null
    val words = view.words ?: return null
    val name = words.firstOrNull()?.substringAfterLast('/')
    return when {
        name in listOf("rg", "grep") -> "Aucune correspondance"
        name == "diff" ||
            (name == "git" &&
                words.getOrNull(1) == "diff" &&
                words.any { it == "--exit-code" || it == "--quiet" }) -> "Différences trouvées"
        else -> null
    }
}

internal fun presentActivity(event: RunEvent): ActivityPresentation {
    val data = event.activityData()
    val item = data?.get("item").asObject()
    val type = item.string("type")
    val result = item?.get("result").asObject()
    val exit = (item?.get("exit_code") as? JsonPrimitive)?.intOrNull
    val hasError =
        item?.get("error")?.let {
            it != JsonNull && it != JsonPrimitive(false) && it != JsonPrimitive("")
        } == true || result?.get("isError") == JsonPrimitive(true)
    val command = commandView(item.string("command"))
    val expected =
        if (type == "command_execution" && !hasError && event.type == "item.completed")
            expectedOutcome(command, exit)
        else null
    val failed =
        expected == null &&
            (hasError ||
                item.string("status") == "failed" ||
                event.type.endsWith("failed") ||
                event.type == "error" ||
                (exit != null && exit != 0) ||
                (event.type == "status" && event.text in listOf("failed", "error")))
    val running =
        !failed && (item.string("status") == "in_progress" || event.type == "item.started")
    val raw = data?.let { activityJson.encodeToString(it) } ?: event.text
    val base =
        ActivityPresentation(
            ActivityKind.NOTICE,
            "Mise à jour",
            state = expected ?: if (failed) "Échec" else if (running) "En cours" else "Terminé",
            failed = failed,
            item = item,
            data = data,
            raw = raw,
            exitCode = exit,
        )
    return when (type) {
        "command_execution" ->
            base.copy(
                kind = command.kind,
                title = command.title,
                subtitle = command.paths.joinToString(" · ").ifBlank { command.source },
                command = command.source,
                paths = command.paths,
                output = item.string("aggregated_output").ifBlank { item.string("output") },
            )
        "saved_output" ->
            base.copy(
                kind = ActivityKind.OUTPUT,
                title = "Résultat enregistré",
                subtitle = "Étape issue de l’historique",
                output = item.string("text"),
            )
        "file_change" ->
            base.copy(
                kind = ActivityKind.FILES,
                title = "Fichiers modifiés",
                subtitle = "${item?.get("changes").asArray()?.size ?: 0} fichier(s)",
            )
        "todo_list" -> base.copy(kind = ActivityKind.PLAN, title = "Plan de travail")
        "reasoning" ->
            base.copy(
                kind = ActivityKind.THINKING,
                title = "Réflexion",
                output = item.string("text"),
            )
        "web_search" ->
            base.copy(
                kind = ActivityKind.SEARCH,
                title = "Recherche sur le web",
                subtitle =
                    item.string("query").ifBlank { item?.get("action").asObject().string("query") },
            )
        "mcp_tool_call" ->
            base.copy(
                kind = ActivityKind.TOOL,
                title =
                    readableField(item.string("tool").substringAfterLast("__")).ifBlank {
                        "Appel d’outil"
                    },
                subtitle = item.string("server"),
            )
        else -> {
            val plain =
                if (data == null) event.text
                else
                    data.string("message").ifBlank {
                        data?.get("error").asObject().string("message")
                    }
            val account = if (event.type == "status") accountUsed(event.text) else null
            val backgroundTasks =
                if (event.type != "turn.waiting") emptyList()
                else
                    data?.get("tasks").asArray().orEmpty().mapNotNull {
                        it.asObject()?.get("description").plain().trim().ifEmpty { null }
                    }
            val title =
                when (event.type) {
                    "thread.started" -> "Session ouverte"
                    "turn.started",
                    "run.started" -> "Travail commencé"
                    "turn.completed" -> "Travail terminé"
                    "turn.waiting" ->
                        if (data?.get("tasks").asArray().isNullOrEmpty())
                            "Tâches en arrière-plan terminées"
                        else "En attente de tâches en arrière-plan"
                    "turn.failed",
                    "error" -> "L’agent a rencontré une erreur"
                    "status" ->
                        when {
                            account != null -> "Compte ${account.first} sélectionné"
                            event.text.lowercase() in
                                listOf(
                                    "succeeded",
                                    "running",
                                    "failed",
                                    "queued",
                                    "cancelled",
                                    "interrupted",
                                ) ->
                                mapOf(
                                        "succeeded" to "Exécution réussie",
                                        "running" to "Exécution en cours",
                                        "failed" to "Exécution en échec",
                                        "queued" to "Exécution en attente",
                                        "cancelled" to "Exécution annulée",
                                        "interrupted" to "Exécution interrompue",
                                    )[event.text.lowercase()]
                                    .orEmpty()
                            else -> plain.take(120).ifBlank { "État de l’exécution" }
                        }
                    "diagnostic" -> "Diagnostic de connexion"
                    "output",
                    "item.completed" -> "Résultat enregistré"
                    "item.started" -> "Étape enregistrée"
                    else -> readableField(type.ifBlank { event.type })
                }
            val usage = data?.get("usage").asObject()
            val subtitle =
                if (account != null) account.second
                else if (backgroundTasks.isNotEmpty()) backgroundTasks.joinToString(" · ")
                else if (usage != null)
                    listOf("input_tokens" to "entrée", "output_tokens" to "sortie")
                        .mapNotNull { (key, label) ->
                            usage[key]?.plain()?.takeIf { it.isNotEmpty() }?.let { "$it $label" }
                        }
                        .joinToString(" · ")
                else ""
            base.copy(
                kind =
                    if (item != null || event.type in listOf("output", "item.completed"))
                        ActivityKind.OUTPUT
                    else ActivityKind.NOTICE,
                title = title,
                subtitle = subtitle,
                output =
                    if (
                        event.type in
                            listOf(
                                "status",
                                "thread.started",
                                "turn.started",
                                "run.started",
                                "turn.completed",
                            )
                    ) {
                        if (data != null) plain
                        else if (event.type == "status") ""
                        else plain.takeUnless { it.startsWith('{') }.orEmpty()
                    } else plain.ifBlank { if (data == null) event.text else "" },
            )
        }
    }
}

internal sealed interface ResultPart {
    data class Text(val value: String) : ResultPart

    data class Data(val value: JsonElement) : ResultPart

    data class Incomplete(val source: String) : ResultPart
}

/**
 * Detect balanced saved JSON, including mixed/fenced output, without inventing truncated fields.
 */
internal fun resultParts(source: String): List<ResultPart> {
    if (source.length > 500_000) return listOf(ResultPart.Text(source))
    val parts = mutableListOf<ResultPart>()
    var start = 0
    var i = 0
    fun parsed(value: String): JsonElement? = runCatching {
        wireJson.parseToJsonElement(value)
    }
        .getOrNull()
        ?.takeIf { it is JsonObject || it is JsonArray }
    fun end(from: Int): Int {
        val stack = mutableListOf<Char>()
        var quote = false
        var escape = false
        for (j in from until source.length) {
            val c = source[j]
            if (quote) {
                if (escape) escape = false
                else if (c == '\\') escape = true else if (c == '"') quote = false
                continue
            }
            if (c == '"') quote = true
            else if (c == '{' || c == '[') {
                stack.add(c)
                if (stack.size > 64) return -2
            } else if (c == '}' || c == ']') {
                if (stack.removeLastOrNull() != if (c == '}') '{' else '[') return -2
                if (stack.isEmpty()) return j + 1
            }
        }
        return -1
    }
    while (i < source.length && parts.size < 100) {
        if (source.startsWith("```", i) || source.startsWith("~~~", i)) {
            val marker = source.substring(i, i + 3)
            val stop = source.indexOf(marker, i + 3)
            val body = source.substring(i + 3, if (stop < 0) source.length else stop)
            val match =
                Regex("^(?:json)?[ \\t]*\\r?\\n([\\s\\S]*)$", RegexOption.IGNORE_CASE)
                    .matchEntire(body)
            if (match != null) {
                val value = parsed(match.groupValues[1])
                if (value != null) {
                    if (i > start) parts.add(ResultPart.Text(source.substring(start, i)))
                    parts.add(ResultPart.Data(value))
                    start = if (stop < 0) source.length else stop + 3
                } else if (
                    match.groupValues[1].trimStart().startsWith('{') ||
                        match.groupValues[1].trimStart().startsWith('[')
                ) {
                    if (i > start) parts.add(ResultPart.Text(source.substring(start, i)))
                    parts.add(ResultPart.Incomplete(match.groupValues[1]))
                    start = if (stop < 0) source.length else stop + 3
                }
            }
            i = if (stop < 0) source.length else stop + 3
            continue
        }
        if (source[i] == '`') {
            val stop = source.indexOf('`', i + 1)
            i = if (stop < 0) source.length else stop + 1
            continue
        }
        if (source[i] in "{[" && (i == 0 || source[i - 1].isWhitespace() || source[i - 1] == ':')) {
            val stop = end(i)
            if (stop > i && (stop == source.length || source[stop] !in "([")) {
                val value = parsed(source.substring(i, stop))
                if (value != null) {
                    if (i > start) parts.add(ResultPart.Text(source.substring(start, i)))
                    parts.add(ResultPart.Data(value))
                    start = stop
                    i = stop
                    continue
                }
            } else if (
                stop < 0 && Regex("^[\\[{]\\s*[\"\\[{]").containsMatchIn(source.substring(i))
            ) {
                if (i > start) parts.add(ResultPart.Text(source.substring(start, i)))
                parts.add(ResultPart.Incomplete(source.substring(i)))
                start = source.length
                break
            } else if (stop < 0) break
        }
        i++
    }
    if (start < source.length) parts.add(ResultPart.Text(source.substring(start)))
    return parts.ifEmpty { listOf(ResultPart.Text(source)) }
}

/** "Using Claude Code account: Studio" → ("Claude Code", "Studio"). */
private fun accountUsed(text: String): Pair<String, String>? =
    Regex("^Using (Codex|Claude Code) account: (.+)$").find(text)?.destructured?.let { (agent, name)
        ->
        agent to name.trim()
    }
