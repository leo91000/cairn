# Validation du stockage à la demande

## Interface locale — PR #5

- `cargo test --locked --lib storage::` : réussi.
- Parcours HTTP export/import existant avec verrou conservé : réussi.
- Typage, lint, `cargo check --locked --workspace --all-targets` et clippy : réussis.
- CI GitHub : https://github.com/leo91000/leo-agent-manager/pull/5 (contrôles réussis).

## Adapter expérimental — deuxième PR

Tests à l'interface de stockage : écriture partielle sans lecture distante préalable,
réouverture, arrêt brutal du processus après acquittement, intégrité des métadonnées
du journal, erreur sur bloc indisponible ou corrompu, réutilisation du cache propre.
Le montage FUSE a aussi été testé directement sur l'hôte avec E/S positionnelles
et synchronisation.

### Essai Firecracker et jailer du 27 septembre 2026

- Firecracker et jailer : 1.17.0, archive vérifiée avec le SHA-256 du Dockerfile.
- Noyau : 6.12.109 extrait de l'image publiée
  `ghcr.io/leo91000/leo-agent-manager@sha256:2db85ce1611e073a4fc6ee292a13aeb25bf10a219130c37975b25c8df2bc12e7`.
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

Ces mesures ne constituent ni un temps de reprise de conversation complète ni un
benchmark S3. Les sections suivantes décrivent les vérifications complémentaires. Les temps
de transfert vers un fournisseur S3 réel ne sont pas mesurés ici.

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
`LEO_NODE_FUSE=1` lors de la génération du déploiement expose `/dev/fuse` ; les
configurations existantes qui l'exposent le conservent. Le superviseur des nodes
expose ce périphérique lorsqu'il est présent sur l'hôte. Les nodes sans FUSE
continuent à prendre en charge les disques historiques.


## Migration et cycle de vie (quatrième PR)

- Une capture de migration garde le disque arrêté sous verrou et lit un lien
  physique vers l'original. Elle ne crée pas une seconde image. Une capture
  abandonnée expire après cinq minutes sans demande de bloc ; ce délai libère le
  verrou de transfert, sans supprimer le disque original.
- L'installation compare de nouveau le disque au manifeste publié. Une écriture
  entre publication et installation fait reporter la migration. Une répétition
  après bascule conserve le journal et ses nouvelles écritures.
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
contrôleur et une vraie VM : nouveau disque journalisé, capture/publication,
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
