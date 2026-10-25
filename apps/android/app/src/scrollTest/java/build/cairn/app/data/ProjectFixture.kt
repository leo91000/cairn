package build.cairn.app.data

// Complete current project values for UI fixtures. Production decodes generated models.
@Suppress("FunctionName")
fun Project(
    id: String = "00000000-0000-4000-8000-000000000001",
    name: String = "",
    path: String = "",
    description: String = "",
    baseBranch: String = "main",
    origin: String? = null,
    sourceMode: String = "remote",
): Project =
    build.cairn.contracts.Project(
        id = id,
        name = name,
        path = path,
        description = description,
        baseBranch = baseBranch,
        origin = origin.orEmpty(),
        sourceMode = sourceMode,
        source = null,
        revision = "",
        createdAt = 0L,
    )
