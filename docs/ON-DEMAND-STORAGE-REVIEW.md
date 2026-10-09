# Revue de la stack de stockage

Base : PR des nodes #3, `635cfd12676676d7415c928289bdb967996105ab`.
Spécification : `ON-DEMAND-STORAGE-DESIGN.md`, confirmée à Q9.
Deux revues indépendantes ont examiné les conventions et la conformité.

## Standards

Aucun constat restant dans le périmètre revérifié.

- Les lectures de statut et ouvertures de journal passent par un pool bloquant
  borné ; la supervision évite les parcours récursifs.
- La recherche d'un disque déjà ouvert ne bloque pas la supervision lorsqu'une
  autre ouverture tient le registre.
- La mise à jour de l'occupation des volumes est commune à la surveillance et à
  la migration.
- La réserve exceptionnelle de publication est documentée et permet de sauvegarder
  le journal quand la réserve normale ne peut plus être satisfaite.

## Spec

Aucun écart matériel restant identifié dans les contre-revues ciblées.

- Les acquittements progressent par génération et par instance de journal. Un
  reçu ancien ne retire ni la base courante ni une publication suivante en cours.
- Le master local activé ne conserve pas un second cache de blocs S3. Les transferts
  de blocs sont bornés et utilisent la mémoire sous pression disque.
- Les pauses de capture et de supervision partagent le même état. Une publication
  d'urgence ne laisse pas les CPU suspendus lorsque les causes d'attente disparaissent.
- L'expiration du droit d'exécution annule les opérations avant toute attente
  bornée du verrou. Le dégel ne conserve plus ce verrou pendant une lecture bloquée.

- Une reprise préempte la capture et la vérification de migration ; une bascule
  déjà engagée termine avant le démarrage. La déconnexion HTTP ne coupe pas cette
  bascule. La contre-revue ciblée n'identifie plus de course sur le verrou du disque.

- L'arrêt forcé annule les lectures FUSE avant d'attendre le VMM. Un invité
  disponible conserve son arrêt gracieux et ses écritures jusqu'à sa sortie.
  La contre-revue ciblée n'identifie pas d'écart de durabilité ou d'ordre d'arrêt.

- La stack intègre les blocs chiffrés binaires de la nouvelle base des nodes,
  avec lecture compatible des anciens blocs. La suppression des audits périodiques
  est conservée ; leurs anciennes erreurs ne forcent plus une restauration complète.
  Les échecs de lecture continuent à invalider les reçus et la base incrémentale.

Bilan : **Standards 0 constat restant ; Spec 0 écart restant identifié**.
La revue ne remplace pas les tests et mesures consignés dans le document de validation.

## Contre-revue externe de `a12ac0e`

- **Bloc S3 absent** : constat valide sur cette version, corrigé par le client
  persistant (`d8ef37a`). Des assertions supplémentaires couvrent le code 409
  depuis S3 et la pause d'intégrité sans attente réseau infinie sur le volume,
  avec conservation des écritures locales.
- **Mesures incomplètes** : constat retenu. Le parcours VM compare maintenant le
  même disque local et à la demande, avec disponibilité, octets, premières
  lectures et débit. Voir le protocole et les limites dans le document de
  validation ; l'origine reste HTTP locale avec délai optionnel, pas un S3 réel.
- **Duplication d'éviction** : observation de maintenabilité examinée ; pas
  d'extraction commune. La node conserve un jeu de blocs propres et sérialise
  l'admission avec les écritures ; le master protège ses copies uniques, exige
  une copie distante vérifiée, retire toutes ses copies S3 redondantes lorsque la
  node locale est activée et autorise une réserve d'urgence pour la publication.
  Mutualiser les suppressions masquerait ces règles ; extraire la seule
  arithmétique créerait une interface presque aussi complexe que son calcul.
  Ces différences sont maintenant explicites près des deux implémentations.

## Revue indépendante via Cairn — 28 septembre 2026

Révisions examinées : nodes `43b9140`, stockage `ab6cf20`, base `c1e402f`.
Le run indépendant `f4908bd7-bd5a-4db7-af0d-562346390f4c` a reproduit quatre
défauts de fonctionnement. Les conclusions antérieures ci-dessus portaient sur
leurs périmètres respectifs ; cette revue étend les scénarios de concurrence
et d'erreurs couverts.

### Standards

- **STD-01** : les synchronisations durables du disque historique utilisent
  maintenant les E/S Tokio. La création et le scellement du journal SQLite
  passent par le pool bloquant borné des journaux. Les barrières de durabilité
  et leur ordre sont conservés lors de la restauration, migration, préparation
  et matérialisation.
- **STD-02**, observation de maintenabilité : les publications avec ou sans
  cache local partagent désormais l'envoi, la vérification distante, le reçu
  et la limite de concurrence. Les conditions d'admission restent distinctes.

### Spec

- **SPEC-A1** : le remplacement prend une réservation dans le même registre
  que les ouvertures. Il refuse avant toute mutation si un volume est encore
  détenu, et refuse les nouvelles ouvertures pendant la bascule. Une inspection
  ne peut donc plus faire réutiliser l'ancien journal après restauration.
- **SPEC-R1** : une réponse de pause perdue laisse l'état CPU indéterminé.
  Une capture normale reprend et dégèle la VM ; une capture d'urgence ou une
  reprise impossible annule l'exécution afin de la faire arrêter.
- **SPEC-R2** : une enveloppe chiffrée invalide, une authentification échouée
  ou un digest incorrect renvoient 409, invalidant la base de sauvegarde et
  déclenchant une attente de résolution explicite sur la node.
- **SPEC-R3** : les refus S3 permanents (accès/configuration) renvoient 424,
  sans invalider une copie dont l'intégrité n'est pas mise en cause. Les erreurs
  temporaires, notamment timeout et throttling, restent réessayables. La node
  conserve les écritures locales et demande une résolution pour un refus permanent.

Les régressions des quatre défauts ont été observées avant correction. Les tests
exercent les interfaces de capture, restauration, lecture de bloc, stockage S3
et volume. Un test sans FUSE couvre aussi l'exclusion des ouvertures pendant
remplacement et la libération de la réservation en cas d'échec.

La contre-revue doit examiner les commits publiés, y compris les nouveaux chemins
de concurrence. La validation avec deux hôtes physiques et un vrai S3 reste une
limite opérationnelle distincte ; elle n'est pas revendiquée par ces fixtures.

### Deuxième tour

Le run `0189eb08-ae33-47e5-93ee-832272b690b4` confirme STD-02, SPEC-A1,
SPEC-R2 et SPEC-R3 dans les chemins examinés. Il relève deux corrections
incomplètes : les barrières de restauration classique dans la PR nodes seule
(STD-01) et les acquittements perdus du moniteur périodique (SPEC-R1).

Les deux barrières restantes de restauration utilisent désormais Tokio. Le
moniteur passe par `Volume::enforce_limits`, appelé sous le verrou de contrôle
de l'exécution. Une erreur de pause ou de reprise annule l'exécution et libère
les lectures bloquées pour permettre son arrêt, tout en conservant le journal.
Le moniteur attend l'installation de l'identité VM avant d'agir sur les CPU.

Les deux pertes d'acquittement ont été reproduites avant correction par les
tests du client de contrôle réel avec un socket Unix simulant Firecracker.
La fixture couvre aussi le démarrage sans identité VM et le cycle normal
pression, pause, libération d'espace, reprise. Le test simulé indépendant et
ces tests HTTP ne constituent pas une validation avec deux hôtes physiques.
