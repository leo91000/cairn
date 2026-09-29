# Journal local segmenté et transitions de protection confirmées

Date : 2026-09-29.

## Problème

La suppression de plusieurs GiB de BLOBs avec SQLite `auto_vacuum=FULL` bloquait
la confirmation locale d'une sauvegarde pendant près d'une minute en production.
Différer le compactage raccourcit le commit mais conserve des pauses importantes
et/ou des pages libres qui occupent toujours la réserve disque. Le contrôle de
santé attendait aussi les verrous de maintenance. Son délai d'une seconde pouvait
demander une suspension, puis une réponse de suspension tardive interrompait la VM.

## Décision

SQLite reste responsable des petites métadonnées : base, générations scellées,
descripteurs de segments, contexte et reçu de publication. Les écritures vont dans
des fichiers de 64 MiB au maximum, appartenant à une seule génération. Chaque
trame contient séquence, génération, position, longueur, date et deux SHA-256 :
l'en-tête est vérifié indépendamment du contenu. Une écriture est confirmée après
`sync_data`, puis intégrée à un index mémoire reconstruit à l'ouverture. Les
lectures fusionnent les index locaux des blocs dans l'ordre des séquences et
s'arrêtent dès que les écritures récentes couvrent leur requête.
Les réécritures d'une même plage dans une même génération remplacent uniquement
son entrée d'index ; les octets physiques et l'ancienneté restent comptés jusqu'à
la publication. Une génération scellée conserve sa propre dernière version.

Un nouveau segment est créé et synchronisé avec son répertoire avant le commit
FULL de son descripteur. Aucune trame n'est confirmée avant ce commit. Le scellement
sérialise les écritures et persiste la génération et le plancher de séquence.
Un append ou un scellement ayant échoué bloque les nouveaux append jusqu'à la
réouverture : un résultat de synchronisation incertain ne peut pas être masqué.

La publication d'une génération draine ses lecteurs, puis commet atomiquement la
nouvelle base, le reçu et la suppression des descripteurs devenus inutiles. Après
ce commit, les index et la base mémoire sont actualisés. Les fichiers sont ensuite
supprimés sans conserver les verrous des lectures ou écritures. Une erreur de
suppression ne transforme pas un reçu durable en refus de publication. L'ouverture
reprend le nettoyage des fichiers sans descripteur. Les données des générations
plus récentes restent toujours possédées. Une confirmation répétée réconcilie
aussi la base mémoire si le précédent commit a perdu sa réponse locale.

À l'ouverture, les trames complètes sont vérifiées ; une queue incomplète est
tronquée uniquement dans le dernier segment non scellé. Une somme incorrecte,
un segment manquant ou une queue tronquée dans une génération scellée fait
échouer l'ouverture. Ces tests établissent la reprise après arrêt de processus ;
ils ne remplacent pas une qualification après perte physique d'alimentation.

## Migration et retour arrière

La migration copie uniquement les métadonnées dans `journal-v2.sqlite`, via un
fichier temporaire synchronisé et renommé. Les BLOBs historiques restent dans
`journal.sqlite`, sont vérifiés et lus directement jusqu'à leur publication.
Les nouveaux append utilisent immédiatement les segments. Dès que la génération
historique est publiée, ses fichiers SQLite sont remplacés par un petit marqueur,
sans compacter ni recopier les BLOBs. Le journal historique est invalidé pour les
anciens lecteurs après la copie durable ; le marqueur empêche ensuite un ancien
binaire de servir une vue périmée. Il faut restaurer une sauvegarde correspondante
des volumes pour revenir à une version antérieure au format v2.

## Contrôleur

Le contrôle de sécurité utilise des compteurs mémoire courts et les signaux de
pression, de source distante et d'intégrité. La réconciliation du cache reste
hors de ce contrôle ; l'admission des écritures conserve la réserve sous son verrou
de node. Une sonde expirée reste unique et est récupérée au contrôle suivant.

Une réponse de transition perdue ou expirée ne constitue pas une confirmation.
Le monitor conserve l'état confirmé et répète la cible actuellement nécessaire,
même lorsque celle-ci a changé depuis la réponse perdue. Les captures répètent la
transition avant le scellement et restent annulables. Un refus explicite ou une
incertitude persistante pendant 30 secondes garde l'arrêt protecteur. Les leases,
les échéances, la pression disque et les erreurs d'intégrité gardent leurs fences.
Une VM protégée mais redevenue saine est reprise avant d'envoyer `freeze`, afin
que ses CPUs puissent exécuter cette commande.

## Validation

Voir [la comparaison des moteurs](../JOURNAL-ENGINE-BENCHMARK.md). La qualification
de la vraie VM s'exécute dans un runner jetable, avec lectures/écritures synchronisées
et sauvegardes répétées :

```sh
LEO_STORAGE_SOAK_SECONDS=300 \
LEO_STORAGE_SOAK_EVIDENCE=/absolute/private/path/storage-vm-soak.json \
node tests/storage-vm-soak.mjs ghcr.io/leo91000/leo-agent-manager@sha256:IMAGE_DIGEST
```

Elle contrôle l'intégrité des lectures après chaque écriture, les latences disque,
les reçus, la progression, la conservation de l'identité de VM et la reprise sur
une nouvelle VM après sauvegarde. L'origine immuable est locale ; les chemins S3
et les conversations réelles nécessitent également leur validation de release.
