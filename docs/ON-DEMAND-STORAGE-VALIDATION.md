# Validation du stockage à la demande

## Interface locale — PR #5

- `cargo test --locked --lib storage::` : réussi.
- Parcours HTTP export/import existant avec verrou conservé : réussi.
- Typage, lint, `cargo check --locked --workspace --all-targets` et clippy : réussis.
- CI GitHub : https://github.com/leo91000/cairn/pull/5 (contrôles réussis).

## Adapter expérimental — deuxième PR

Tests à l'interface de stockage : écriture partielle sans lecture distante préalable,
réouverture, arrêt brutal du processus après acquittement, intégrité des métadonnées
du journal, erreur sur bloc indisponible ou corrompu, réutilisation du cache propre.
Le montage FUSE a aussi été testé directement sur l'hôte avec E/S positionnelles
et synchronisation.

### Essai Firecracker et jailer du 27 septembre 2026

- Firecracker et jailer : 1.17.0, archive vérifiée avec le SHA-256 du Dockerfile.
- Noyau : 6.12.109 extrait de l'image publiée
  `ghcr.io/leo91000/cairn@sha256:2db85ce1611e073a4fc6ee292a13aeb25bf10a219130c37975b25c8df2bc12e7`.
- Couche noyau vérifiée :
  `sha256:66155c1a271c7d0c39f1c3bdb5749a677290633711f4eb918d4f5f1496880144`.
- Invité minimal BusyBox avec ext4, données inutilisées de 32 Mio, cache initial vide.
- Compilation debug ; source locale de blocs immuables, **pas S3**.

Résultat :

```json
{ "coldFetchedBytes": 16777216, "guestReadyMs": 5761, "guestSyncSurvivedKill": true, "remoteNonzeroBytes": 71303168, "virtualDiskBytes": 268435456 }
```

Le marqueur de disponibilité est émis après lecture, modification et synchronisation
d'un fichier invité. Le test tue ensuite Firecracker, ferme le montage, rouvre le
journal, matérialise le disque, rejoue le journal ext4 et vérifie le contenu écrit.
Le téléchargement complet n'a lieu que lors de cette matérialisation de contrôle.

Le premier essai avait échoué à la réouverture parce que le thread FUSE détenait
encore le journal après le démontage. `MountedDisk` attend maintenant la fin de ce
thread avant de rendre le disque réouvrable ; le scénario complet est passé ensuite.

Reproduction (Linux, KVM, FUSE, privilèges de montage, BusyBox statique, e2fsprogs,
Firecracker/jailer installés aux emplacements du projet) :

```sh
tests/lazy-disk-smoke.sh /chemin/vers/vmlinux
```

### Annulation pendant une lecture distante bloquée

Le parcours CI complet a révélé un ordre d'arrêt incorrect : le contrôleur
attendait la sortie du VMM avant d'annuler ses lectures FUSE. Le scénario réduit
`lazy_disk_smoke --cancel` appelle le vrai `Vm::boot` et bloque son premier accès
à une origine indisponible. Avant correction : dépassement du délai de 12 secondes.
Après correction : **89 ms** pour annuler et libérer le VMM, le montage et le verrou.
Le script de reproduction ci-dessus exécute également ce scénario.
Les invités disponibles conservent l'arrêt gracieux ; les E/S sont annulées avant
un arrêt forcé, avec conservation du journal durable.

Ces mesures ne constituent ni un temps de reprise de conversation complète ni un
benchmark S3. Les sections suivantes décrivent les vérifications complémentaires. Les temps
de transfert vers un fournisseur S3 réel ne sont pas mesurés ici.

### Admission du cache propre et VM préparée — 28 septembre 2026

Le remplissage d'un bloc refaisait deux parcours des fichiers propres : un
parcours de tous les disques pour appliquer le budget de la node, puis un
parcours du cache de la conversation même quand ce budget avait déjà admis le
bloc. L'index en mémoire suit désormais la taille et l'ancienneté des fichiers
propres, est reconstruit au premier accès, puis réconcilié par le moniteur au
plus une fois par minute. Les écritures et les remplissages gardent le même
verrou d'admission ; aucun journal n'est candidat à l'éviction. Un test à
l'interface de `LazyDisk` vérifie que la lecture d'un bloc propre le rend plus
récent et que l'arrivée d'une troisième conversation évince le bon fichier.

Banc ciblé `large_clean_cache_admission_performance` : 8 000 fichiers propres
valides de 4 Kio, budget 100 Gio, six admissions successives sans éviction.
Sur le même environnement et en compilation debug, **avant** : 47–101 ms par
admission ; **après** : 0,007–0,015 ms pour les cinq admissions suivant la
construction de l'index. Cette première construction a pris **218 ms**. Les
petits fichiers isolent le coût des métadonnées ; ce n'est pas une mesure du
débit de blocs de 4 Mio ni un p95 de production. Le moniteur peut construire
l'index avant la prochaine conversation. La réconciliation périodique permet
de détecter les fichiers modifiés hors du contrôleur.

Le même script Firecracker/jailer exécute aussi `--prepared` : la VM démarre
avec un disque provisoire de 4 Mio ; l'invité annonce qu'il attend avant tout
montage ou E/S sur ce disque. Un montage FUSE anonyme est présent **avant**
le démarrage du jailer. Après l'annonce, le test lui attribue un `LazyDisk`,
remplace le disque provisoire par `PATCH /drives/data`, attend le changement de
taille visible dans l'invité, puis monte ext4, lit, écrit et synchronise.
Il confirme zéro lecture avant attribution, 12 Mio de blocs lus avant le
marqueur final sur 71 Mio non nuls, et la survie de l'écriture après arrêt
brutal, réouverture du journal et contrôle ext4. Un montage FUSE créé **après**
le jailer a échoué : sa namespace de montage ne voit pas le nouveau chemin.

Deux essais sur la même fixture ont donné **4 701 et 4 738 ms** pour le démarrage
classique. Le scénario préparé a atteint son marqueur d'attente à **3 301 et
3 093 ms**, puis le marqueur après synchronisation **1 498 et 1 394 ms après
attribution**. Le coût de préparation est donc déplacé avant la demande, pas
éliminé. C'est une VM BusyBox minimale avec une source de blocs locale ; les
chiffres ne prédisent pas le gain du vrai parcours conversation, ni la latence
S3. L'init de production monte immédiatement le disque de données pour son
overlay : il doit évoluer avant qu'une VM générique puisse utiliser ce mécanisme.
Les outils ne peuvent pas encore être chauffés dans cette VM sans disque. Le
pool de production reste donc inchangé en attendant une mesure complète avec
le vrai invité, l'annulation, plusieurs tailles de disque et le coût RAM du
stock de VM préparées. Voir la [documentation Firecracker 1.17](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/api_requests/patch-block.md)
pour la condition de ne monter ni lire le disque pendant son remplacement.
Le script vérifie aussi l'annulation d'un démarrage froid bloqué sur une lecture
distante (92 ms dans le dernier essai) ; l'annulation au milieu d'une attribution
de VM préparée reste à vérifier lors de son éventuelle intégration.

## Publication et pression disque (troisième PR)

Les générations du journal séparent le point envoyé des écritures reçues pendant son
transfert. La publication conserve les anciens blocs accessibles jusqu'à ce que le
contrôleur ait drainé les lectures et confirmé le nouveau manifeste. La rétention
respecte ces références, y compris après un transfert interrompu.

Une capture normale gèle le système de fichiers, scelle la génération et reprend
l'invité avant de reconstruire les blocs. Si l'exécution est déjà bloquée par la
réserve disque ou le retard de sauvegarde, la capture d'urgence est explicitement
marquée `crash` : elle capture le préfixe durable du journal avec les CPU en pause.
Elle demande la relecture du journal ext4 à la reprise. Attendre `fsfreeze` dans ce
cas pourrait attendre une écriture elle-même bloquée par le manque d'espace et
empêcher la sauvegarde de libérer cet espace. Ce point n'offre pas de cohérence
applicative supplémentaire. Une panne de lecture distante reporte la capture.

L'activation vérifie la configuration S3 existante (notamment l'absence de règle de
cycle de vie indépendante) et réalise un montage, une écriture et une
synchronisation FUSE avec l'identité du contrôleur. Sur les installations Compose,
le déploiement expose `/dev/fuse` au runner, y compris pour les configurations
existantes qui ne l'exposaient pas. Le superviseur des nodes expose également ce
périphérique lorsqu'il est présent sur l'hôte. Les nouvelles conversations exigent
FUSE sur toutes les nodes.


## Migration et cycle de vie (quatrième PR)

- Une capture de migration garde le disque arrêté sous verrou et lit un lien
  physique vers l'original. Elle ne crée pas une seconde image. Une capture
  abandonnée expire après cinq minutes sans demande de bloc ; ce délai libère le
  verrou de transfert, sans supprimer le disque original.
- L'installation compare de nouveau le disque au manifeste publié. Une écriture
  entre publication et installation fait reporter la migration. Une répétition
  après bascule conserve le journal et ses nouvelles écritures. Une reprise
  préempte la capture ou la vérification ; si la bascule est déjà engagée, elle
  attend sa fin. Le test d'interface présente une annulation avant vérification et confirme
  que l'original et son verrou sont immédiatement réutilisables.
- Les nouveaux environnements sont formatés puis journalisés avant la première
  commande utilisateur. Un redémarrage au milieu de cette préparation conserve
  une source récupérable.
- L'agrandissement exceptionnel matérialise explicitement une image pour les
  outils ext4 existants, après vérification de l'espace nécessaire. Il peut donc
  être long et demander davantage de disque ; il ne suit pas le démarrage à la
  demande. L'ancien journal reste en copie de secours jusqu'au nettoyage explicite.
- Les blocs S3 sont téléchargés dans un fichier mémoire anonyme borné, sans
  temporaire disque. La publication peut utiliser au plus quelques blocs sous la
  réserve normale pour sortir d'une pression disque ; elle exige encore 32 Mio
  libres plus trois fois le bloc chiffré en cours.

Vérifications effectuées :

- Suite backend complète : **248 réussites**, quatre tests optionnels ignorés.
- Frontend : **260 réussites** dans 37 fichiers ; vérification TypeScript réussie.
- Suite S3 chiffrée contre Moto **5.2.3** : envoi, vérification, relais sortant,
  réparation d'objet manquant, rétention et purge. Le scénario supplémentaire
  impose une réserve impossible à satisfaire : la publication et la lecture par
  mémoire réussissent sans conserver un second cache sur le master local.
- Test HTTP des droits : ancien propriétaire refusé, deux instances séparées,
  reçu ancien ignoré et publication suivante conservée pendant la réconciliation
  d'un acquittement perdu.
- Interface du journal : durabilité, générations concurrentes, panne et annulation,
  corruption, réserve disque et reprise, migration sans téléchargement.
- Vrai Firecracker/jailer : résultat ci-dessus, refait sur le code final.
- Revue Standards et Spec : aucun constat restant après corrections ; voir
  [le compte rendu](ON-DEMAND-STORAGE-REVIEW.md).

La CI de la dernière PR exécute aussi `tests/runner-storage-smoke.mjs` avec le vrai
contrôleur et une vraie VM : reprise préemptant une capture de migration,
nouveau disque journalisé, capture/publication,
restauration dans un répertoire vide, lecture distante en panne puis annulée,
reprise sans lire les données inutilisées, pause effective des CPU sous pression,
publication d'urgence puis reprise automatique. Son origine de blocs est HTTP en
boucle locale. Le résultat de ce parcours et les contrôles de l'image sont visibles
sur la PR ; ce document ne revendique pas un benchmark avec un fournisseur S3.

Sur les nodes locales activées, le master transfère les blocs en mémoire et nettoie
ses anciennes copies S3 évictables ; le seul cache propre persistant est celui du
contrôleur. Les anciennes sauvegardes dont le master est la seule copie restent
protégées. Les remplissages du cache et les écritures du journal partagent le même
verrou d'admission d'espace sur le contrôleur.

## Intégration de la base des nodes actualisée

La stack est remise sur `635cfd12676676d7415c928289bdb967996105ab` : nouveaux blocs
chiffrés binaires, anciens blocs toujours lisibles, audits planifiés supprimés.
Le test de sélection du point de reprise reproduit une erreur d'audit historique
qui empêchait la reprise à la demande ; cette métadonnée retirée n'influence plus
la sélection. L'invalidation après un échec de lecture reste en place.

Le parcours contrôleur/VM complet est passé avant cette remise à jour :
restauration de métadonnées en **21 ms**, reprise du programme invité en **1 711 ms**,
**4 blocs lus sur 13 blocs distants**, annulation en panne et reprise après pression
disque réussies. Origine HTTP locale, pas un benchmark S3. La CI des branches
réactualisées fournit la validation finale de l'intégration.

## Contre-revue de la PR #8 après le client S3 persistant

La revue externe portait sur `a12ac0e`. Depuis `d8ef37a`, `NoSuchKey` ou HTTP 404
renvoie **409** depuis le master ; une panne de transport renvoie **503**. Le
contrôleur termine la lecture sur 409 et conserve son état `integrity` jusqu'à
résolution explicite, tandis que 503 reste une attente annulable avec reprise.

La couverture vérifie maintenant explicitement :

- contre Moto, l'absence distante sans cache donne 409, retire le reçu de copie et
  invalide la base incrémentale ; une nouvelle capture répare la copie ;
- à l'interface du volume, une réponse 409 produit une seule requête, un échec de
  lecture et un état `integrity` qui demande la pause ; une lecture locale réussie
  n'efface pas cet état ; le travail journalisé survit à la réouverture ;
- le test existant d'indisponibilité 503 attend, récupère quand l'origine revient
  et reste annulable ; une panne S3 transitoire conserve reçu et base incrémentale.

### Protocole comparatif de performances

`node tests/runner-smoke.mjs <image>` exécute désormais, avec le vrai contrôleur,
Firecracker et jailer, trois modes sur **le même manifeste et les mêmes données** :

1. `local-full` : restauration complète avant démarrage, disque local historique ;
2. `demand-http` : restauration des métadonnées, cache vide, blocs lus à la demande ;
3. `demand-http-delayed` : identique, avec 50 ms supplémentaires par requête de bloc.

Chaque mode est répété trois fois, en alternant l'ordre. Chaque échantillon utilise
un répertoire de disque vide et une nouvelle exécution ; le cache de pages de
l'hôte n'est pas purgé. Les images de runtime sont déjà installées. Le cache propre
à la demande est limité à 8 Mio ; le cache mémoire du contrôleur et la relecture
anticipée de l'invité restent ceux du code livré.

Les lignes JSON `benchmark: conversation-disk` rapportent :

- `restoreMs`, puis `bootMs`, et leur somme `availableMs` jusqu'au marqueur invité
  après lecture correcte du fichier sauvegardé (observation à 100 ms près) ;
- `bytesAtReady` et `bytesAfterReads` : octets des blocs servis, **en comptant les
  répétitions**, restauration comprise ; en mode à la demande, la disponibilité
  doit précéder le téléchargement complet ;
- `savedReadMs` : lecture du petit fichier de reprise dans l'invité ;
- `firstReadsMs` : trois premières lectures de 4 Kio aux offsets 0, 8 et 16 Mio
  du fichier de 32 Mio inutilisé pendant le démarrage ;
- `sequentialMs` et `mibPerSecond` : lecture complète de ces 32 Mio après les trois
  sondes. Cette phase est donc partiellement chaude, dans tous les modes. Son
  SHA-256 est vérifié après la fenêtre mesurée.

Le contrôleur donne le signal de mesure à l'invité seulement après relevé des
compteurs de démarrage. Les horloges de chaque durée restent sur la même machine.
L'origine HTTP est en boucle locale : le délai injecté ne simule ni une limite de
bande passante ni un réseau WAN complet. Ces chiffres comparent les parcours disque,
**pas la latence d'un fournisseur S3 ni les limites maximales de l'architecture**.
