package dev.leo.manager.ui

import dev.leo.manager.data.Task

private val weekdays =
    mapOf(
        "0" to "dimanche",
        "7" to "dimanche",
        "1" to "lundi",
        "2" to "mardi",
        "3" to "mercredi",
        "4" to "jeudi",
        "5" to "vendredi",
        "6" to "samedi",
        "SUN" to "dimanche",
        "MON" to "lundi",
        "TUE" to "mardi",
        "WED" to "mercredi",
        "THU" to "jeudi",
        "FRI" to "vendredi",
        "SAT" to "samedi",
    )

private fun clock(minute: String, hour: String): String? {
    val m = minute.toIntOrNull()?.takeIf { it in 0..59 } ?: return null
    val h = hour.toIntOrNull()?.takeIf { it in 0..23 } ?: return null
    return "%02d:%02d".format(h, m)
}

/**
 * Human wording for the common cron shapes produced by the editor and typed by hand. Anything else
 * stays visible as the raw expression rather than being guessed.
 */
internal fun describeCron(cron: String): String {
    val parts = cron.trim().split(Regex("\\s+"))
    if (parts.size != 5) return "Cron · ${cron.trim()}"
    val (minute, hour, day, month, weekday) = parts
    if (month != "*") return "Cron · ${cron.trim()}"
    if (hour == "*" && day == "*" && weekday == "*") {
        if (minute == "*") return "Chaque minute"
        minute
            .removePrefix("*/")
            .toIntOrNull()
            ?.takeIf { minute.startsWith("*/") }
            ?.let {
                return "Toutes les $it minutes"
            }
        if (minute.toIntOrNull() != null)
            return if (minute == "0") "Toutes les heures"
            else "Toutes les heures à :${minute.padStart(2, '0')}"
    }
    val time = clock(minute, hour) ?: return "Cron · ${cron.trim()}"
    val upper = weekday.uppercase()
    return when {
        day == "*" && weekday == "*" -> "Tous les jours · $time"
        day == "*" && (upper == "1-5" || upper == "MON-FRI") -> "En semaine · $time"
        day == "*" && (upper == "0,6" || upper == "6,0" || upper == "SAT,SUN" || upper == "6-7") ->
            "Le week-end · $time"
        day == "*" && weekdays.containsKey(upper) -> "Chaque ${weekdays.getValue(upper)} · $time"
        day == "*" && upper.split(',').all { weekdays.containsKey(it) } ->
            "Les " +
                upper.split(',').map { weekdays.getValue(it) }.distinct().joinToString(", ") +
                " · $time"
        weekday == "*" && day.toIntOrNull()?.let { it in 1..31 } == true ->
            (if (day == "1") "Le 1er du mois" else "Le $day du mois") + " · $time"
        else -> "Cron · ${cron.trim()}"
    }
}

internal fun describeSchedule(task: Task): String =
    when {
        task.archived -> "Archivée"
        task.cron == null -> "Ponctuelle"
        !task.enabled -> "En pause · ${describeCron(task.cron)}"
        else -> describeCron(task.cron)
    }
