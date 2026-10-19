package build.cairn.app.data

/** Older pages belong to one displayed history/epoch, independently of stream collection. */
internal class OlderPages {
    private var events = emptyList<RunEvent>()
    private var history: String? = null
    private var epoch = 0
    private var oldest: Long? = null
    private var hasOlder = false

    private fun revalidate(snapshot: LiveSnapshot) {
        if (snapshot.history != history || snapshot.epoch != epoch) {
            events = emptyList()
            oldest = null
            history = snapshot.history
            epoch = snapshot.epoch
        }
    }

    fun display(snapshot: LiveSnapshot): LiveSnapshot {
        revalidate(snapshot)
        val boundary = oldest ?: return snapshot
        return snapshot.copy(
            events = mergeHistory(events, snapshot.events),
            oldest = boundary,
            hasOlder = hasOlder,
        )
    }

    /** Return null for a response from a history that has since been replaced. */
    fun apply(
        page: HistoryPage,
        snapshot: LiveSnapshot,
        requested: LiveSnapshot,
    ): LiveSnapshot? {
        if (
            snapshot.history != requested.history ||
                snapshot.epoch != requested.epoch ||
                page.history != requested.history
        )
            return null

        require(page.oldest < requested.oldest || !page.hasOlder) {
            "Page d’historique invalide."
        }

        revalidate(snapshot)
        events = mergeHistory(page.events, events)
        oldest = page.oldest
        hasOlder = page.hasOlder
        return display(snapshot)
    }
}
