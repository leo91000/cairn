# Simplification de la publication S3

Décision utilisateur du 28 septembre 2026, applicable au-dessus de la PR nodes dans la PR stockage.

## Comportement attendu

- Un seul mécanisme publie les nouveaux états de disque vers S3. Aucune nouvelle destination master ni rétention historique configurable.
- Écritures durables localement, publication asynchrone visée toutes les 60 secondes, pause après 5 minutes de travail non synchronisé, comme auparavant.
- Le pointeur de publication fait autorité, même en cas de timestamps identiques ou de recul d’horloge. Aucun retour silencieux à une ancienne génération si l’état courant est endommagé.
- Les blocs communs sont partagés. Seuls l’état courant et les générations effectivement référencées par des lecteurs ou déplacements restent protégés. Nettoyage possible sans nouvelle écriture et après redémarrage.
- Une publication incomplète conserve l’état courant et le journal local. Un manifeste nécessaire manquant ou endommagé bloque la collecte destructive.
- Supprimer création/restauration d’archives, transitions froides, réglages et vues d’archives Web/Android, transferts export/import dédiés et code devenu inutile.
- Conserver la corbeille (30 jours), la révocation des liens publics, l’annulation du travail et la suppression sûre des disques.
- Nettoyage de production demandé séparément : supprimer les conversations archivées et vider le bucket OVH après vérification des références actives. Aucun merge/déploiement implicite.

## Architecture

Le module `nodes::publication` porte la capture, publication vérifiée et collecte. Le module `object_storage` porte les accès S3 partagés. `conversation_deletion` ne porte que l’effacement des fichiers, disques et enregistrements. Les clés persistées `node-backups`, les noms de champs du protocole disque et les anciens alias de configuration S3 restent compatibles pour éviter une migration destructive des disques existants.

## Vérifications

Tests aux interfaces existantes : HTTP conversation et contrôleur, publication/lecture/collecte du disque, parcours de déplacement via relais avec S3 de test. Couvrir absence d’archivage malgré une ancienne politique, purge de corbeille, blocs partagés, lecteurs encore présents, suppression après libération, horloge non monotone, corruption et publication interrompue. Exécuter la suite normale et `tests/node_s3_test.py` ; les parcours qui publiaient auparavant vers le master requièrent désormais cette validation S3 explicite. Les interruptions après PUT et avant enregistrement sont nettoyables après redémarrage et lors de la suppression de conversation, même sans reçu local.
