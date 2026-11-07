# Une configuration native commune aux deux modes d’édition

Les réglages natifs de Codex, Claude Code et OpenCode ont des valeurs par défaut
pour l’installation et des personnalisations par agent Cairn. Un formulaire et
un éditeur de texte natif modifient la même configuration : le formulaire conserve
les champs qu’il ne représente pas. Le texte d’un agent montre la configuration
effective, distingue les valeurs héritées et permet de revenir à l’héritage sans
figer les autres défauts de l’installation.

Les réglages déjà présents dans Cairn restent synchronisés avec les champs natifs
correspondants, sans deux sources de vérité concurrentes. Les droits Cairn sont
le plafond : une configuration native peut les restreindre, jamais les élargir.
Une option incompatible avec le fonctionnement de l’adaptateur est refusée avec
une explication avant l’enregistrement, jamais acceptée puis ignorée. Les
identifiants des comptes et les jetons temporaires ne figurent pas dans le texte.

Une modification s’applique au prochain message, y compris dans une conversation
existante, sans interrompre un tour actif. Si son application exige de recréer une
session native, l’interface l’annonce avant l’application. L’ouverture des options
natives ne réactive pas implicitement les fonctionnalités désactivées par les
ADR-0013 et ADR-0042 : toute prise en charge supplémentaire doit respecter les
contrats de reprise, de permissions et de gestion des comptes de Cairn.
