from infrastructure import DeploymentConfig, create_infrastructure


create_infrastructure(DeploymentConfig.from_pulumi())
