package build.cairn.app.data

import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl

/** Accept only a request belonging to the connected workspace; never fetch pasted URLs. */
fun authorizationParameters(input: String, origin: HttpUrl): Map<String, String> {
    val url = input.trim().toHttpUrl()
    require(
        url.scheme == origin.scheme &&
            url.host == origin.host &&
            url.port == origin.port &&
            url.username.isEmpty() &&
            url.password.isEmpty() &&
            url.fragment == null
    ) {
        "Le lien doit appartenir au serveur auquel vous êtes connecté."
    }
    require(url.encodedPath in setOf("/authorize", "/oauth/authorize")) {
        "Ce lien n’est pas une demande d’autorisation Cairn."
    }
    require(url.queryParameterNames.all { url.queryParameterValues(it).size == 1 }) {
        "Le lien contient des paramètres en double."
    }
    return url.queryParameterNames.associateWith { url.queryParameter(it).orEmpty() }
}
