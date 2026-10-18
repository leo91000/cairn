@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import android.Manifest
import android.content.ContentUris
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Build
import android.provider.MediaStore
import android.util.Size
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Read access to photos: full, limited to the user's selection (Android 14+), or none. */
private fun photoPermissions(): Array<String> =
    when {
        Build.VERSION.SDK_INT >= 34 ->
            arrayOf(
                Manifest.permission.READ_MEDIA_IMAGES,
                Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED,
            )
        Build.VERSION.SDK_INT >= 33 -> arrayOf(Manifest.permission.READ_MEDIA_IMAGES)
        else -> arrayOf(Manifest.permission.READ_EXTERNAL_STORAGE)
    }

private fun canReadPhotos(context: Context): Boolean =
    photoPermissions().any {
        ContextCompat.checkSelfPermission(context, it) == PackageManager.PERMISSION_GRANTED
    }

/** Limited access lets the user add more photos to the selection Cairn may read. */
private fun limitedPhotos(context: Context): Boolean =
    Build.VERSION.SDK_INT >= 34 &&
        ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED,
        ) == PackageManager.PERMISSION_GRANTED &&
        ContextCompat.checkSelfPermission(context, Manifest.permission.READ_MEDIA_IMAGES) !=
            PackageManager.PERMISSION_GRANTED

private suspend fun recentPhotos(context: Context, limit: Int = 24): List<Uri> =
    withContext(Dispatchers.IO) {
        val photos = mutableListOf<Uri>()
        context.contentResolver
            .query(
                MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
                arrayOf(MediaStore.Images.Media._ID),
                null,
                null,
                "${MediaStore.Images.Media.DATE_ADDED} DESC",
            )
            ?.use { cursor ->
                while (cursor.moveToNext() && photos.size < limit) photos +=
                    ContentUris.withAppendedId(
                        MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
                        cursor.getLong(0),
                    )
            }
        photos
    }

internal suspend fun photoThumbnail(context: Context, uri: Uri): Bitmap? =
    withContext(Dispatchers.IO) {
        runCatching {
            if (Build.VERSION.SDK_INT >= 29)
                context.contentResolver.loadThumbnail(uri, Size(256, 256), null)
            else {
                val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                context.contentResolver.openInputStream(uri)?.use {
                    BitmapFactory.decodeStream(it, null, bounds)
                }
                val sample =
                    Integer.highestOneBit(
                        (maxOf(bounds.outWidth, bounds.outHeight) / 256).coerceAtLeast(1)
                    )
                context.contentResolver.openInputStream(uri)?.use {
                    BitmapFactory.decodeStream(
                        it,
                        null,
                        BitmapFactory.Options().apply { inSampleSize = sample },
                    )
                }
            }
        }
            .getOrNull()
    }

/**
 * The ＋ menu of the composer: recent photos to pick in one tap, the camera, the system photo picker
 * and files. `remaining` is how many files the message may still take.
 */
@Composable
internal fun AttachSheet(
    remaining: Int,
    attach: (uris: List<Uri>, staged: () -> Unit) -> Unit,
    close: () -> Unit,
) {
    val context = LocalContext.current
    var allowed by remember { mutableStateOf(canReadPhotos(context)) }
    var limited by remember { mutableStateOf(limitedPhotos(context)) }
    var permissionRevision by remember { mutableIntStateOf(0) }
    var photos by remember { mutableStateOf<List<Uri>>(emptyList()) }
    var selected by remember { mutableStateOf<List<Uri>>(emptyList()) }
    // Survives the camera activity recreating this one.
    var capture by rememberSaveable { mutableStateOf<String?>(null) }
    fun done(uris: List<Uri>, staged: () -> Unit = {}) {
        close()
        if (uris.isNotEmpty()) attach(uris, staged)
    }
    val permission =
        rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
            allowed = canReadPhotos(context)
            limited = limitedPhotos(context)
            permissionRevision++
        }
    LaunchedEffect(allowed, limited, permissionRevision) {
        val available = if (allowed) recentPhotos(context) else emptyList()
        photos = available
        selected = selected.filter { it in available }
    }
    val camera =
        rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { taken ->
            val file = capture?.let(::File)
            // The photo is copied into the draft, then its capture file is removed.
            if (taken && file != null)
                done(
                    listOf(
                        FileProvider.getUriForFile(context, "${context.packageName}.files", file)
                    )
                ) {
                    file.delete()
                }
            else file?.delete()
        }
    val gallery =
        rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(8)) {
            done(it)
        }
    val documents =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) {
            done(it)
        }
    ModalBottomSheet(onDismissRequest = close) {
        Column(Modifier.fillMaxWidth().padding(bottom = 24.dp).testTag("attach-sheet")) {
            LazyRow(
                Modifier.fillMaxWidth().padding(bottom = 8.dp),
                contentPadding = PaddingValues(horizontal = 20.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                if (!allowed || limited)
                    item {
                        PhotoAccessTile(
                            if (limited) "Choisir d’autres photos" else "Photos récentes"
                        ) {
                            permission.launch(photoPermissions())
                        }
                    }
                items(photos, key = { it.toString() }) { uri ->
                    val chosen = uri in selected
                    RecentPhoto(uri, chosen, enabled = chosen || selected.size < remaining) {
                        selected = if (chosen) selected - uri else selected + uri
                    }
                }
            }
            if (selected.isNotEmpty())
                Button(
                    onClick = { done(selected) },
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 4.dp),
                ) {
                    Text(
                        if (selected.size == 1) "Joindre 1 photo"
                        else "Joindre ${selected.size} photos"
                    )
                }
            if (context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY))
                AttachmentAction(CairnIcons.Camera, "Appareil photo") {
                    val stamp = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.ROOT).format(Date())
                    val file =
                        File(context.cacheDir, "cairn-files/photo-$stamp.jpg").apply {
                            parentFile?.mkdirs()
                        }
                    capture = file.path
                    camera.launch(
                        FileProvider.getUriForFile(context, "${context.packageName}.files", file)
                    )
                }
            AttachmentAction(CairnIcons.Image, "Photos et vidéos") {
                gallery.launch(
                    PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo)
                )
            }
            AttachmentAction(CairnIcons.Folder, "Fichiers") { documents.launch(arrayOf("*/*")) }
        }
    }
}

@Composable
private fun PhotoAccessTile(label: String, onClick: () -> Unit) {
    Column(
        Modifier.size(76.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .clickable(onClickLabel = "Autoriser l’accès aux photos", onClick = onClick)
            .padding(6.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(CairnIcons.Image, null, Modifier.size(22.dp))
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            textAlign = TextAlign.Center,
            maxLines = 2,
        )
    }
}

@Composable
private fun RecentPhoto(uri: Uri, chosen: Boolean, enabled: Boolean, toggle: () -> Unit) {
    val context = LocalContext.current
    val bitmap by produceState<Bitmap?>(null, uri) { value = photoThumbnail(context, uri) }
    Box(
        Modifier.size(76.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .then(
                if (chosen)
                    Modifier.border(
                        3.dp,
                        MaterialTheme.colorScheme.primary,
                        RoundedCornerShape(12.dp),
                    )
                else Modifier
            )
            .clickable(enabled = enabled, onClick = toggle)
            .semantics {
                contentDescription = "Photo récente"
                selected = chosen
            }
    ) {
        bitmap?.let {
            Image(
                it.asImageBitmap(),
                null,
                Modifier.matchParentSize(),
                contentScale = ContentScale.Crop,
            )
        }
        if (chosen)
            Box(
                Modifier.align(Alignment.TopEnd)
                    .padding(5.dp)
                    .size(20.dp)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.primary),
                contentAlignment = Alignment.Center,
            ) {
                Icon(
                    Icons.Default.Check,
                    null,
                    Modifier.size(14.dp),
                    tint = MaterialTheme.colorScheme.onPrimary,
                )
            }
        else if (!enabled)
            Box(Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.35f)))
    }
}
