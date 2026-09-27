# Validation du stockage à la demande

## Interface locale — PR #5

- `cargo test --locked --lib storage::` : réussi.
- Parcours HTTP export/import existant avec verrou conservé : réussi.
- Typage, lint, `cargo check --locked --workspace --all-targets` et clippy : réussis.
- CI GitHub : https://github.com/leo91000/leo-agent-manager/pull/5 (statut final à vérifier).

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
{ "coldFetchedBytes": 16777216, "guestReadyMs": 15594, "guestSyncSurvivedKill": true, "remoteNonzeroBytes": 71303168, "virtualDiskBytes": 268435456 }
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
benchmark S3. Restent à valider dans les PR suivantes : publication distante,
limites globales par node, états d'attente et annulation, droits et conservation des
objets, déplacement, migration, archives et fonctionnement avec le vrai stockage S3.
L'adapter n'est pas encore activable dans le contrôleur de production.
