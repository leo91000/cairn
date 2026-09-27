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
